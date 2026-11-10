//! Connection to the host: vsock, virtio-serial, or a Unix socket in tests.

use std::fs::File;
use std::io::{Read, Write};
#[cfg(unix)]
use std::net::Shutdown;
use std::path::PathBuf;
use std::time::Duration;

use ternvale_agent_proto::AGENT_SERIAL_NAME;

#[cfg(windows)]
use crate::windows_vsock;
use crate::AgentError;

/// Where the host agent server listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// AF_VSOCK address. The host is CID 2. Linux and Windows (`viosock`).
    Vsock {
        /// Context id.
        cid: u32,
        /// Port.
        port: u32,
    },
    /// virtio-console named port (`vioserial` / `virtio_console`).
    Serial {
        /// Port name, default [`AGENT_SERIAL_NAME`].
        name: String,
    },
    /// Try vsock, then serial (Windows service default).
    Auto {
        /// vsock CID.
        cid: u32,
        /// vsock port.
        port: u32,
        /// Serial port name.
        serial: String,
    },
    /// A Unix socket, e.g. the host side `host-5000.sock` in tests.
    #[cfg(unix)]
    Unix(PathBuf),
}

impl Target {
    /// Host CID 2, port 5000, serial [`AGENT_SERIAL_NAME`].
    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
    pub fn auto_default() -> Self {
        Self::Auto {
            cid: 2,
            port: ternvale_agent_proto::AGENT_PORT,
            serial: AGENT_SERIAL_NAME.into(),
        }
    }
}

impl std::fmt::Display for Target {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Vsock { cid, port } => write!(formatter, "vsock:{cid}:{port}"),
            Self::Serial { name } => write!(formatter, "serial:{name}"),
            Self::Auto { cid, port, serial } => {
                write!(formatter, "auto:vsock:{cid}:{port},serial:{serial}")
            }
            #[cfg(unix)]
            Self::Unix(path) => write!(formatter, "unix:{}", path.display()),
        }
    }
}

/// Byte pipe used by a session. Implementations are vsock, serial, or Unix.
pub trait Transport: Read + Write + Send {
    /// Short name for logs (`vsock`, `serial`, `unix`).
    fn name(&self) -> &'static str;
    /// Bound `SO_RCVTIMEO` when the OS supports it.
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()>;
    /// Shut the pipe down (best effort).
    fn close(&self);
}

/// A connected stream.
#[derive(Debug)]
pub enum Stream {
    /// Unix socket.
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    /// vsock stream.
    #[cfg(target_os = "linux")]
    Vsock(vsock::VsockStream),
    /// Windows `viosock` Winsock.
    #[cfg(windows)]
    Vsock(windows_vsock::WinVsock),
    /// virtio-serial named port.
    Serial(File),
}

impl Transport for Stream {
    fn name(&self) -> &'static str {
        match self {
            #[cfg(unix)]
            Self::Unix(_) => "unix",
            #[cfg(target_os = "linux")]
            Self::Vsock(_) => "vsock",
            #[cfg(windows)]
            Self::Vsock(_) => "vsock",
            Self::Serial(_) => "serial",
        }
    }

    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.set_read_timeout(timeout),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => stream.set_read_timeout(timeout),
            #[cfg(windows)]
            Self::Vsock(stream) => stream.set_read_timeout(timeout),
            Self::Serial(_) => Ok(()),
        }
    }

    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    fn close(&self) {
        let result = match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.shutdown(Shutdown::Both),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => stream.shutdown(Shutdown::Both),
            #[cfg(windows)]
            Self::Vsock(stream) => stream.shutdown(),
            Self::Serial(_) => Ok(()),
        };
        if let Err(error) = result {
            tracing::trace!(target: "ternvale::agent", error = %error, "shutdown of agent stream failed");
        }
    }
}

impl Stream {
    /// Short name for logs (`vsock`, `serial`, `unix`).
    pub fn name(&self) -> &'static str {
        Transport::name(self)
    }

    /// Set `SO_RCVTIMEO`.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        Transport::set_read_timeout(self, timeout)
    }

    /// Shut both directions down (best effort; the peer may be gone).
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn close(&self) {
        Transport::close(self);
    }

    /// Duplex clone for writing `ExecOutput` off the session thread.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn try_clone(&self) -> std::io::Result<Self> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => Ok(Self::Unix(stream.try_clone()?)),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => Ok(Self::Vsock(stream.try_clone()?)),
            #[cfg(windows)]
            Self::Vsock(_) => Err(std::io::Error::other("vsock clone is not supported")),
            Self::Serial(file) => Ok(Self::Serial(file.try_clone()?)),
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.read(buf),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => stream.read(buf),
            #[cfg(windows)]
            Self::Vsock(stream) => stream.read(buf),
            Self::Serial(file) => file.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.write(buf),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => stream.write(buf),
            #[cfg(windows)]
            Self::Vsock(stream) => stream.write(buf),
            Self::Serial(file) => file.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(stream) => stream.flush(),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => stream.flush(),
            #[cfg(windows)]
            Self::Vsock(stream) => stream.flush(),
            Self::Serial(file) => file.flush(),
        }
    }
}

/// Connect to `target`.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(addr = %target))]
pub(crate) fn connect(target: &Target) -> Result<Stream, AgentError> {
    match target {
        Target::Auto { cid, port, serial } => match connect(&Target::Vsock {
            cid: *cid,
            port: *port,
        }) {
            Ok(stream) => Ok(stream),
            Err(vsock_err) => {
                tracing::info!(
                    target: "ternvale::agent",
                    error = %vsock_err,
                    "vsock unavailable; trying virtio-serial"
                );
                connect(&Target::Serial {
                    name: serial.clone(),
                })
            }
        },
        Target::Serial { name } => connect_serial(name),
        #[cfg(unix)]
        Target::Unix(path) => {
            let stream = std::os::unix::net::UnixStream::connect(path).map_err(|source| {
                AgentError::Connect {
                    target: target.to_string(),
                    source,
                }
            })?;
            tracing::debug!(target: "ternvale::agent", addr = %target, transport = "unix", "connected to host");
            Ok(Stream::Unix(stream))
        }
        Target::Vsock { cid, port } => connect_vsock(*cid, *port, target),
    }
}

#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(cid, port)
)]
fn connect_vsock(cid: u32, port: u32, target: &Target) -> Result<Stream, AgentError> {
    let failed = |source| AgentError::Connect {
        target: target.to_string(),
        source,
    };
    #[cfg(target_os = "linux")]
    {
        let stream = vsock::VsockStream::connect_with_cid_port(cid, port).map_err(failed)?;
        tracing::debug!(target: "ternvale::agent", addr = %target, transport = "vsock", "connected to host");
        return Ok(Stream::Vsock(stream));
    }
    #[cfg(windows)]
    {
        let stream = windows_vsock::connect(cid, port).map_err(failed)?;
        tracing::debug!(target: "ternvale::agent", addr = %target, transport = "vsock", "connected to host");
        return Ok(Stream::Vsock(stream));
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        let _ = (cid, port, failed);
        Err(AgentError::Unsupported("vsock"))
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(name))]
fn connect_serial(name: &str) -> Result<Stream, AgentError> {
    let path = serial_path(name);
    let file = File::options()
        .read(true)
        .write(true)
        .open(&path)
        .map_err(|source| AgentError::Connect {
            target: format!("serial:{name}"),
            source,
        })?;
    tracing::debug!(
        target: "ternvale::agent",
        path = %path.display(),
        transport = "serial",
        "connected to host"
    );
    Ok(Stream::Serial(file))
}

fn serial_path(name: &str) -> PathBuf {
    #[cfg(windows)]
    {
        PathBuf::from(format!(r"\\.\Global\{name}"))
    }
    #[cfg(not(windows))]
    {
        PathBuf::from(format!("/dev/virtio-ports/{name}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_default_uses_host_cid_and_named_port() {
        assert_eq!(
            Target::auto_default(),
            Target::Auto {
                cid: 2,
                port: 5000,
                serial: AGENT_SERIAL_NAME.into(),
            }
        );
        assert_eq!(
            Target::auto_default().to_string(),
            "auto:vsock:2:5000,serial:org.ternvale.agent"
        );
        assert_eq!(
            Target::Serial {
                name: AGENT_SERIAL_NAME.into()
            }
            .to_string(),
            "serial:org.ternvale.agent"
        );
    }
}
