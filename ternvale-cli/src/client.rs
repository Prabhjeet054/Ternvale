//! Control socket client for `status`, `pause`, `resume`, and `stop`.

use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};

use crate::protocol::{Request, Response, MAX_PAUSE_MS};

/// Longest response line read back (stats for 16 CPUs fit easily).
const MAX_RESPONSE: u64 = 64 * 1024;

/// Why a VM could not be reached.
#[derive(Debug, PartialEq, Eq)]
pub enum Reach {
    /// No socket file.
    NotRunning,
    /// A socket file nobody serves (the VM process died).
    Stale,
}

/// Connect to `path`, or say why there is no VM behind it.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
pub fn connect(path: &Path) -> Result<std::result::Result<UnixStream, Reach>> {
    if std::fs::symlink_metadata(path).is_err() {
        return Ok(Err(Reach::NotRunning));
    }
    match UnixStream::connect(path) {
        Ok(stream) => Ok(Ok(stream)),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            tracing::debug!(target: "ternvale::cli", "control socket refused the connection");
            Ok(Err(Reach::Stale))
        }
        Err(error) => {
            Err(error).with_context(|| format!("connect to control socket {}", path.display()))
        }
    }
}

/// Send one request on `stream` and read its response.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(cmd = request.name()))]
pub fn exchange(stream: UnixStream, request: &Request) -> Result<Response> {
    let wait = Duration::from_millis(MAX_PAUSE_MS + 10_000);
    stream
        .set_read_timeout(Some(wait))
        .context("set control read timeout")?;
    let mut writer = stream.try_clone().context("clone control connection")?;
    let mut line = serde_json::to_string(request).context("encode control request")?;
    line.push('\n');
    writer
        .write_all(line.as_bytes())
        .with_context(|| format!("send {} request", request.name()))?;
    let mut reply = String::new();
    BufReader::new(stream)
        .take(MAX_RESPONSE)
        .read_line(&mut reply)
        .with_context(|| format!("read {} response", request.name()))?;
    if reply.is_empty() {
        bail!(
            "the vm closed the control socket without answering {}",
            request.name()
        );
    }
    let response: Response = serde_json::from_str(reply.trim_end())
        .with_context(|| format!("parse {} response {reply:?}", request.name()))?;
    tracing::debug!(target: "ternvale::cli", ok = response.ok, "control response");
    Ok(response)
}

/// Connect to VM `name` at `path` and send `request`. A VM that is not
/// running is an error naming the path.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name, cmd = request.name()))]
pub fn request(path: &Path, name: &str, request: &Request) -> Result<Response> {
    match connect(path)? {
        Ok(stream) => exchange(stream, request),
        Err(Reach::NotRunning) => bail!(
            "cannot {} vm {name}: it is not running (no control socket at {})",
            request.verb(),
            path.display()
        ),
        Err(Reach::Stale) => bail!(
            "cannot {} vm {name}: it is not running (stale control socket at {}; its process died)",
            request.verb(),
            path.display()
        ),
    }
}
