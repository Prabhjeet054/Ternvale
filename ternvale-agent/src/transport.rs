//! Connection to the host: AF_VSOCK in the guest, or a Unix socket.

use std::io::{Read, Write};
use std::net::Shutdown;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::Duration;

use crate::AgentError;

/// Where the host agent server listens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// AF_VSOCK address (Linux only). The host is CID 2.
    Vsock {
        /// Context id.
        cid: u32,
        /// Port.
        port: u32,
    },
    /// A Unix socket, e.g. the host side `host-5000.sock` in tests.
    Unix(PathBuf),
}

impl std::fmt::Display for Target {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Vsock { cid, port } => write!(formatter, "vsock:{cid}:{port}"),
            Self::Unix(path) => write!(formatter, "unix:{}", path.display()),
        }
    }
}

/// A connected stream.
#[derive(Debug)]
pub enum Stream {
    /// Unix socket.
    Unix(UnixStream),
    /// vsock stream.
    #[cfg(target_os = "linux")]
    Vsock(vsock::VsockStream),
}

impl Stream {
    /// Set `SO_RCVTIMEO`.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> std::io::Result<()> {
        match self {
            Self::Unix(stream) => stream.set_read_timeout(timeout),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => stream.set_read_timeout(timeout),
        }
    }

    /// Shut both directions down (best effort; the peer may be gone).
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn close(&self) {
        let result = match self {
            Self::Unix(stream) => stream.shutdown(Shutdown::Both),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => stream.shutdown(Shutdown::Both),
        };
        if let Err(error) = result {
            tracing::trace!(target: "ternvale::agent", error = %error, "shutdown of agent stream failed");
        }
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Self::Unix(stream) => stream.read(buf),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => stream.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Unix(stream) => stream.write(buf),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => stream.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Self::Unix(stream) => stream.flush(),
            #[cfg(target_os = "linux")]
            Self::Vsock(stream) => stream.flush(),
        }
    }
}

/// Connect to `target`.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(addr = %target))]
pub(crate) fn connect(target: &Target) -> Result<Stream, AgentError> {
    let failed = |source| AgentError::Connect {
        target: target.to_string(),
        source,
    };
    let stream = match target {
        Target::Unix(path) => Stream::Unix(UnixStream::connect(path).map_err(failed)?),
        #[cfg(target_os = "linux")]
        Target::Vsock { cid, port } => {
            Stream::Vsock(vsock::VsockStream::connect_with_cid_port(*cid, *port).map_err(failed)?)
        }
        #[cfg(not(target_os = "linux"))]
        Target::Vsock { .. } => return Err(AgentError::Unsupported("vsock")),
    };
    tracing::debug!(target: "ternvale::agent", addr = %target, "connected to host");
    Ok(stream)
}
