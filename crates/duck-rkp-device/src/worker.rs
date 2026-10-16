use std::{
    fs,
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use duck_core::AppPaths;
use serde::{Deserialize, Serialize};

use crate::{
    error::DeviceRkpError,
    protocol::{self, BUFFER_SIZE, MAX_REQUEST_SIZE},
};

const TIMEOUT: Duration = Duration::from_secs(30);
const MAX_REPLY_FILE: u64 = (BUFFER_SIZE * 6) as u64;

#[derive(Serialize, Deserialize)]
enum Reply {
    Payload(Vec<u8>),
    Error { code: String, message: String },
}

/// Vendor calls cannot be interrupted safely inside the CLI process. A child owns
/// the session, stdout is discarded, and the parent kills and reaps it on timeout.
pub(crate) fn sign(paths: &AppPaths, request: &[u8]) -> Result<Vec<u8>> {
    // Anything around the child process is a local transport failure, not an internal
    // error: the WebUI has a message for `ta_transport_error` and none for the latter.
    let local = |error: anyhow::Error| {
        DeviceRkpError::Transport(format!("device RKP worker failed: {error:#}"))
    };
    fs::create_dir_all(&paths.tmp_dir)
        .with_context(|| format!("create {}", paths.tmp_dir.display()))
        .map_err(local)?;
    let exchange = tempfile::Builder::new()
        .prefix("rkp-device-")
        .tempdir_in(&paths.tmp_dir)
        .context("create the worker exchange directory")
        .map_err(local)?;
    fs::write(exchange.path().join("request.bin"), request)
        .context("write the worker request")
        .map_err(local)?;
    let executable = std::env::current_exe()
        .context("locate this executable")
        .map_err(local)?;
    let mut command = Command::new(&executable);
    command
        .args(["rkp-device", "ta-csr", "--exchange"])
        .arg(exchange.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    detach_from_terminal(&mut command);
    let child = command
        .spawn()
        .with_context(|| format!("start the KeyMint worker ({})", executable.display()))
        .map_err(local)?;
    let mut worker = Worker(child);
    let status = worker.wait(TIMEOUT)?;
    let reply_path = exchange.path().join("reply.json");
    if !status.success() || !reply_path.is_file() {
        return Err(DeviceRkpError::Transport(format!(
            "worker exited with {status}; vendor ABI may be incompatible"
        ))
        .into());
    }
    if fs::metadata(&reply_path)
        .with_context(|| format!("read {}", reply_path.display()))
        .map_err(local)?
        .len()
        > MAX_REPLY_FILE
    {
        return Err(DeviceRkpError::InvalidResponse("worker reply too large".into()).into());
    }
    let reply = fs::read(&reply_path)
        .with_context(|| format!("read {}", reply_path.display()))
        .map_err(local)?;
    match serde_json::from_slice(&reply)
        .context("parse the worker reply")
        .map_err(|error| DeviceRkpError::InvalidResponse(format!("{error:#}")))?
    {
        Reply::Payload(payload) if payload.len() <= BUFFER_SIZE => Ok(payload),
        Reply::Payload(_) => {
            Err(DeviceRkpError::InvalidResponse("payload too large".into()).into())
        }
        Reply::Error { code, message } if code == "ta_rejected" => {
            let status = message.parse().context("invalid TA status from worker")?;
            Err(DeviceRkpError::Rejected(status).into())
        }
        Reply::Error { message, .. } => Err(DeviceRkpError::Transport(message).into()),
    }
}

/// Puts the child in its own session, so a Ctrl-C aimed at the CLI (or anything else
/// signalling the process group) cannot interrupt the vendor call halfway through. The
/// null stdio above leaves the new session with nothing open on the terminal.
///
/// This is the crate's only `unsafe`: there is no safe API for `setsid` between fork and
/// exec, and killing the child mid-call is worse than the risk of calling it.
#[cfg(unix)]
#[allow(unsafe_code)]
fn detach_from_terminal(command: &mut Command) {
    use std::os::unix::process::CommandExt as _;

    // SAFETY: `setsid` is async-signal-safe and one of the few calls documented as safe
    // between fork and exec. A failure aborts the child before it touches vendor code.
    unsafe {
        command.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
fn detach_from_terminal(_command: &mut Command) {}

pub(crate) fn execute(exchange: &Path) -> Result<()> {
    let result = (|| {
        let path = exchange.join("request.bin");
        if fs::metadata(&path)?.len() > MAX_REQUEST_SIZE as u64 {
            bail!("worker request too large");
        }
        let request = fs::read(path)?;
        protocol::ta_payload(&request)?;
        native_request(&request)
    })();
    let reply = match result {
        Ok(payload) => Reply::Payload(payload),
        Err(error) => {
            let message = match error.downcast_ref::<DeviceRkpError>() {
                Some(DeviceRkpError::Rejected(status)) => status.to_string(),
                _ => format!("{error:#}"),
            };
            Reply::Error {
                code: crate::error::code_of(&error).into(),
                message,
            }
        }
    };
    let encoded = serde_json::to_vec(&reply).context("encode the worker reply")?;
    fs::write(exchange.join("reply.json"), encoded)
        .context("write the worker reply")
        .map_err(|error| DeviceRkpError::InvalidResponse(format!("{error:#}")))?;
    Ok(())
}

fn native_request(request: &[u8]) -> Result<Vec<u8>> {
    #[cfg(all(target_os = "android", target_arch = "aarch64"))]
    {
        crate::keymint::request(request)
    }
    #[cfg(not(all(target_os = "android", target_arch = "aarch64")))]
    {
        let _ = request;
        Err(DeviceRkpError::UnsupportedDevice("Android arm64 required".into()).into())
    }
}

struct Worker(Child);

impl Worker {
    fn wait(&mut self, timeout: Duration) -> Result<ExitStatus> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.0.try_wait()? {
                return Ok(status);
            }
            if Instant::now() >= deadline {
                return Err(DeviceRkpError::Timeout.into());
            }
            thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    /// A Ctrl-C aimed at the CLI reaches the whole foreground process group, so the worker
    /// must leave it or the vendor call would be interrupted halfway through. `setsid`
    /// that succeeds makes the child its own session *and* group leader, which `ps` can
    /// report on every platform this runs on.
    #[test]
    fn the_worker_leaves_the_callers_process_group() {
        let mut command = Command::new("sh");
        command.args(["-c", "exec sleep 30"]);
        detach_from_terminal(&mut command);
        let child = command.spawn().unwrap();
        let pid = child.id();
        let _reaper = Worker(child);

        let listing = Command::new("ps")
            .args(["-o", "pgid=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let group = String::from_utf8_lossy(&listing.stdout).trim().to_owned();

        assert_eq!(
            group,
            pid.to_string(),
            "the worker must lead its own process group"
        );
    }

    /// A setup failure must be a documented code, not the generic `internal_error`.    /// A setup failure must be a documented code, not the generic `internal_error`.
    #[test]
    fn a_broken_exchange_directory_is_a_transport_error() {
        let dir = tempfile::tempdir().unwrap();
        // A file where the temporary directory should be.
        let blocked = dir.path().join("tmp");
        fs::write(&blocked, b"not a directory").unwrap();
        let paths = AppPaths::for_root(dir.path(), dir.path());
        let paths = AppPaths {
            tmp_dir: blocked,
            ..paths
        };

        let error = sign(&paths, &[0x22, 0x31, 0x00, 0x00]).unwrap_err();
        assert_eq!(crate::error::code_of(&error), "ta_transport_error");
        assert!(error.to_string().contains("worker failed"));
    }

    /// A worker that exits cleanly without a reply is a transport failure too.
    #[test]
    fn a_worker_that_writes_no_reply_is_a_transport_error() {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::for_root(dir.path(), dir.path());
        fs::create_dir_all(&paths.tmp_dir).unwrap();

        // The real executable rejects this request before it ever writes a reply, which is
        // exactly the "worker exited without a reply" path.
        let error = sign(&paths, &[0x22, 0x31, 0x00, 0x00]).unwrap_err();
        assert_eq!(crate::error::code_of(&error), "ta_transport_error");
    }

    #[test]
    fn hung_worker_times_out_and_is_reaped() {
        let child = Command::new("sh")
            .args(["-c", "exec sleep 30"])
            .spawn()
            .unwrap();
        let pid = child.id();
        {
            let mut worker = Worker(child);
            let error = worker.wait(Duration::from_millis(10)).unwrap_err();
            assert_eq!(crate::error::code_of(&error), "ta_timeout");
        }
        let output = Command::new("kill")
            .args(["-0", &pid.to_string()])
            .output()
            .unwrap();
        assert!(!output.status.success());
    }
}
