//! Selects one device protocol while preserving each protocol's validation semantics.
use crate::{error::DeviceRkpError, protocol, status::DeviceStatus, v1};
use anyhow::{Context, Result};
use clap::ValueEnum;
use duck_rkp::{cose::EcKeyPair, http::EekResponse, verify::VerifyReport};
use serde::Serialize;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    #[default]
    Auto,
    V1,
    V2,
}

/// Device adapters preserve each protocol's distinct request and response contract.
pub(crate) trait Exchange {
    fn csr(&self, request: &[u8]) -> Result<Vec<u8>>;
    fn csr_v1(&self, _request: &v1::Request) -> Result<v1::Response> {
        Err(DeviceRkpError::UnsupportedDevice("V1 adapter is unavailable".into()).into())
    }
}

pub(crate) fn blockers_for(status: &DeviceStatus, protocol: Protocol) -> Vec<&'static str> {
    let mut blockers = status.blockers.clone();
    // The status report describes both alternatives; an explicit protocol must not
    // require the other adapter's resources. All shared and unknown blockers remain.
    blockers.retain(|blocker| match protocol {
        Protocol::V1 => !matches!(
            *blocker,
            "keymint_library_missing" | "smcinvoke_missing" | "v2_unavailable"
        ),
        Protocol::V2 => *blocker != "v1_unavailable",
        Protocol::Auto => true,
    });
    let missing = match protocol {
        Protocol::V1 if !status.v1_available => Some("v1_unavailable"),
        Protocol::V2 if !status.v2_available => Some("v2_unavailable"),
        _ => None,
    };
    if let Some(missing) = missing.filter(|missing| !blockers.contains(missing)) {
        blockers.push(missing);
    }
    blockers
}

pub(crate) struct SignedCsr {
    pub bytes: Vec<u8>,
    pub protocol: Protocol,
    /// V1 evidence is encrypted for the server, so only V2 has a local signature report.
    pub local_verify: Option<VerifyReport>,
}

pub(crate) fn issue(
    selected: Protocol,
    status: &DeviceStatus,
    exchange: &dyn Exchange,
    key: &EcKeyPair,
    eek: &EekResponse,
) -> Result<SignedCsr> {
    if selected == Protocol::V1 || (selected == Protocol::Auto && !status.v2_available) {
        return issue_v1(exchange, key, eek);
    }
    let request = protocol::build_request(&eek.challenge, &key.cose_public)?;
    let signed = match exchange.csr(&request) {
        Ok(signed) => signed,
        Err(error) => {
            let unsupported = error.chain().any(|cause| {
                matches!(
                    cause.downcast_ref::<DeviceRkpError>(),
                    Some(DeviceRkpError::Rejected(-40))
                )
            });
            if selected == Protocol::Auto && status.v1_available && unsupported {
                return issue_v1(exchange, key, eek)
                    .context("CSR_V2 returned -40; V1 fallback failed");
            }
            return Err(error);
        }
    };
    let bytes = protocol::unwrap_csr(&signed)?;
    let report = protocol::validate_csr(&bytes, &eek.challenge, &key.cose_public)?;
    Ok(SignedCsr {
        bytes,
        protocol: Protocol::V2,
        local_verify: Some(report),
    })
}

fn issue_v1(exchange: &dyn Exchange, key: &EcKeyPair, eek: &EekResponse) -> Result<SignedCsr> {
    let request = v1::request(&eek.challenge, &key.cose_public, &eek.eek_chains)?;
    let response = exchange.csr_v1(&request)?;
    let bytes = v1::assemble(response, &eek.challenge, &key.cose_public)
        .map_err(|error| DeviceRkpError::InvalidCsr(format!("V1: {error:#}")))?;
    Ok(SignedCsr {
        bytes,
        protocol: Protocol::V1,
        local_verify: None,
    })
}
