//! The device-backed provisioning round trip.
//!
//! Unlike the offline workbench, this path never touches a seed: the challenge and the
//! certificate chain come from Google, the CSR is signed by the device's own KeyMint TA,
//! and only the private key stays on the host. Both sides of the exchange are traits so
//! the whole flow can be exercised without a device or a network.

use std::path::PathBuf;

use anyhow::{Context as _, Result, bail};
use duck_core::BoxFuture;
use duck_core::{Context, fs::write_bytes_atomic};
use duck_platform::net::http_client;
use duck_rkp::{
    cose::{EcKeyPair, generate_ec_keypair},
    http::{EekResponse, fetch_eek, submit_csr},
    profile::{DEFAULT_GENERIC_FINGERPRINT, DEFAULT_SERVER_URL},
    verify::VerifyReport,
};
use reqwest::Client;
use serde::Serialize;

use crate::{
    artifacts::{self, ArtifactPaths, Artifacts, ChainSummary},
    error::DeviceRkpError,
    protocol::{self, MAX_CERT_CHAIN_LEN},
    status::{self, DeviceStatus, LIMITATIONS},
    worker,
};

/// Everything the caller can still adjust without touching KeyMint.
#[derive(Debug, Clone, Default)]
pub(crate) struct Options {
    /// Override the build fingerprint sent to `fetchEekChain`.
    pub fingerprint: Option<String>,
    /// Also write `keybox.xml` here, instead of only inside the timestamped run directory.
    pub output: Option<PathBuf>,
}

/// Where the key pair that ends up in `keysToSign` comes from. Production uses the OS
/// RNG; a test substitutes a fixed pair so it can issue a certificate for it.
pub(crate) trait KeySource {
    fn generate(&self) -> Result<EcKeyPair>;
}

/// The production key source.
pub(crate) struct RandomKeys;

impl KeySource for RandomKeys {
    fn generate(&self) -> Result<EcKeyPair> {
        generate_ec_keypair()
    }
}

/// The vendor half of the round trip: one `CSR_V2` command in an isolated child process.
pub(crate) trait Exchange {
    fn csr(&self, request: &[u8]) -> Result<Vec<u8>>;
}

/// The capability report the run is gated on. Only tests replace it; the device answer
/// comes from `status::detect`.
pub(crate) trait Environment {
    fn capabilities(&self, ctx: &Context) -> DeviceStatus;
}

/// Everything the running device actually offers.
pub(crate) struct DeviceEnvironment;

impl Environment for DeviceEnvironment {
    fn capabilities(&self, ctx: &Context) -> DeviceStatus {
        status::detect(ctx)
    }
}

/// The Google half: the anti-replay challenge and the issued certificate chain. The
/// futures are boxed so the trait stays usable behind a reference.
pub(crate) trait Server: Sync {
    fn challenge<'a>(&'a self, fingerprint: &'a str) -> BoxFuture<'a, Result<EekResponse>>;
    fn certificates<'a>(
        &'a self,
        csr: &'a [u8],
        challenge: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<Vec<u8>>>>;
}

/// The real vendor path. It runs inside a child process started from this binary.
pub(crate) struct Device;

impl Exchange for Device {
    fn csr(&self, request: &[u8]) -> Result<Vec<u8>> {
        let paths = duck_core::AppPaths::discover()?;
        worker::sign(&paths, request)
    }
}

/// The real server path, through the shared HTTP client.
#[derive(Debug)]
pub(crate) struct Google {
    client: Client,
    server_url: String,
}

impl Google {
    pub(crate) fn new(server_url: Option<String>) -> Result<Self> {
        Ok(Self {
            client: http_client()?,
            server_url: server_url.unwrap_or_else(|| DEFAULT_SERVER_URL.to_owned()),
        })
    }
}

impl Server for Google {
    fn challenge<'a>(&'a self, fingerprint: &'a str) -> BoxFuture<'a, Result<EekResponse>> {
        Box::pin(async move { fetch_eek(&self.client, fingerprint, &self.server_url).await })
    }

    fn certificates<'a>(
        &'a self,
        csr: &'a [u8],
        challenge: &'a [u8],
    ) -> BoxFuture<'a, Result<Vec<Vec<u8>>>> {
        Box::pin(async move { submit_csr(&self.client, csr, challenge, &self.server_url).await })
    }
}

#[derive(Debug, Serialize)]
pub(crate) struct ProvisionData {
    /// The run only happened because the caller acknowledged the limitations.
    pub acknowledged_limitations: bool,
    /// Stable identifiers of the limitations the user acknowledged.
    pub limitations: [&'static str; 2],
    /// How the CSR was produced, so a report can tell a device run from an offline one.
    pub mode: &'static str,
    pub fingerprint: String,
    pub challenge_hex: String,
    pub csr_len: usize,
    pub local_verify: VerifyReport,
    pub chain: ChainSummary,
    pub device_id: String,
    pub keybox_xml: String,
    pub artifacts: ArtifactPaths,
    pub prerequisites: DeviceStatus,
}

/// Runs one complete provisioning round trip and writes every artifact.
pub(crate) async fn provision(
    ctx: &Context,
    acknowledged: bool,
    options: Options,
    key_source: &dyn KeySource,
    exchange: &dyn Exchange,
    server: &dyn Server,
    environment: &dyn Environment,
) -> Result<ProvisionData> {
    if !acknowledged {
        return Err(DeviceRkpError::AcknowledgementRequired.into());
    }

    let prerequisites = environment.capabilities(ctx);
    if !prerequisites.prerequisites_met {
        return Err(DeviceRkpError::UnsupportedDevice(prerequisites.blockers.join(", ")).into());
    }

    let fingerprint = resolve_fingerprint(options.fingerprint.as_deref());
    let key = key_source.generate()?;
    let eek = server.challenge(&fingerprint).await?;

    let request = protocol::build_request(&eek.challenge, &key.cose_public)?;
    let signed = exchange.csr(&request)?;
    let csr = protocol::unwrap_csr(&signed)?;
    let local_verify = protocol::validate_csr(&csr, &eek.challenge, &key.cose_public)?;

    let chains = server.certificates(&csr, &eek.challenge).await?;
    let chain_der = first_chain(&chains)?;
    protocol::validate_leaf_key(&key.secret_key, &chain_der)?;

    let mut artifacts = artifacts::write(
        &ctx.paths,
        artifacts::Inputs {
            secret_key: &key.secret_key,
            csr: &csr,
            cert_chain_der: &chain_der,
            manufacturer: &prerequisites.soc_manufacturer,
        },
    )?;
    if let Some(path) = options.output {
        write_extra_keybox(&path, &artifacts.keybox_xml)?;
        artifacts.paths.keybox_path = path.display().to_string();
    }

    Ok(report(
        &fingerprint,
        &eek,
        &csr,
        local_verify,
        artifacts,
        prerequisites,
    ))
}

fn report(
    fingerprint: &str,
    eek: &EekResponse,
    csr: &[u8],
    local_verify: VerifyReport,
    artifacts: Artifacts,
    prerequisites: DeviceStatus,
) -> ProvisionData {
    ProvisionData {
        acknowledged_limitations: true,
        limitations: LIMITATIONS,
        mode: "device-ta-oracle",
        fingerprint: fingerprint.to_owned(),
        challenge_hex: eek.challenge_hex.clone(),
        csr_len: csr.len(),
        local_verify,
        chain: ChainSummary::of(&artifacts.chain),
        device_id: artifacts.device_id.clone(),
        keybox_xml: artifacts.keybox_xml.clone(),
        artifacts: artifacts.paths.clone(),
        prerequisites,
    }
}

/// Mirrors the keybox into the path the caller asked for, so their own tooling can pick
/// it up where it expects it.
fn write_extra_keybox(path: &std::path::Path, xml: &str) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    write_bytes_atomic(path, xml.as_bytes())
        .with_context(|| format!("write {}", path.display()))?;
    Ok(())
}

/// The fingerprint the server binds the challenge to. The device's own value is the
/// honest default; the generic one only keeps a host-side dry run alive.
fn resolve_fingerprint(explicit: Option<&str>) -> String {
    if let Some(value) = explicit.map(str::trim).filter(|value| !value.is_empty()) {
        return value.to_owned();
    }
    let detected = duck_rkp::profile::detect::detect().fingerprint.value;
    if detected.trim().is_empty() {
        DEFAULT_GENERIC_FINGERPRINT.to_owned()
    } else {
        detected
    }
}

fn first_chain(chains: &[Vec<u8>]) -> Result<Vec<u8>> {
    let chain = chains
        .first()
        .context("RKP server returned no certificate chains")?;
    if chain.len() > MAX_CERT_CHAIN_LEN {
        bail!("issued certificate chain is larger than {MAX_CERT_CHAIN_LEN} bytes");
    }
    Ok(chain.to_vec())
}

#[cfg(test)]
#[path = "service_tests.rs"]
mod tests;
