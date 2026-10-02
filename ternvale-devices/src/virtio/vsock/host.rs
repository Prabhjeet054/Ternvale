//! Host side of vsock: one Unix domain socket per port.
//!
//! - Guest connects to host port P: Ternvale connects to `<dir>/host-P.sock`,
//!   which a host service (for example the future agent server) listens on.
//! - Host connects to guest port P: a host process connects to
//!   `<dir>/guest-P.sock`, which Ternvale listens on for each configured port.

use std::io::ErrorKind;
use std::os::unix::fs::FileTypeExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use super::VsockError;

/// Socket a host service listens on to accept guest connections to `port`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtio::vsock",
    skip(dir),
    fields(port)
)]
pub fn host_socket_path(dir: &Path, port: u32) -> PathBuf {
    dir.join(format!("host-{port}.sock"))
}

/// Socket Ternvale listens on; connecting to it opens a stream to guest `port`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::virtio::vsock",
    skip(dir),
    fields(port)
)]
pub fn guest_socket_path(dir: &Path, port: u32) -> PathBuf {
    dir.join(format!("guest-{port}.sock"))
}

pub(super) struct HostSide {
    dir: PathBuf,
    listeners: Vec<(u32, UnixListener, PathBuf)>,
}

impl HostSide {
    /// Create `dir` and bind a nonblocking listener for each port in `ports`.
    pub(super) fn open(dir: &Path, ports: &[u32]) -> Result<Self, VsockError> {
        std::fs::create_dir_all(dir).map_err(|source| uds_error(dir, source))?;
        let mut listeners = Vec::with_capacity(ports.len());
        for &port in ports {
            let path = guest_socket_path(dir, port);
            remove_stale_socket(&path)?;
            let listener = UnixListener::bind(&path).map_err(|source| uds_error(&path, source))?;
            listener
                .set_nonblocking(true)
                .map_err(|source| uds_error(&path, source))?;
            tracing::info!(
                target: "ternvale::virtio::vsock",
                port,
                path = %path.display(),
                "vsock host listener ready"
            );
            listeners.push((port, listener, path));
        }
        Ok(Self {
            dir: dir.to_path_buf(),
            listeners,
        })
    }

    /// Connect to the host service for guest connections to `port`.
    pub(super) fn connect(&self, port: u32) -> std::io::Result<UnixStream> {
        let path = host_socket_path(&self.dir, port);
        let stream = UnixStream::connect(&path)?;
        stream.set_nonblocking(true)?;
        Ok(stream)
    }

    pub(super) fn host_path(&self, port: u32) -> PathBuf {
        host_socket_path(&self.dir, port)
    }

    /// Accept every pending host connection as `(guest_port, stream)`.
    pub(super) fn accept(&mut self) -> Vec<(u32, UnixStream)> {
        let mut out = Vec::new();
        for (port, listener, path) in &self.listeners {
            loop {
                match listener.accept() {
                    Ok((stream, _)) => match stream.set_nonblocking(true) {
                        Ok(()) => out.push((*port, stream)),
                        Err(error) => tracing::warn!(
                            target: "ternvale::virtio::vsock",
                            port,
                            error = %error,
                            "could not make accepted vsock stream nonblocking; dropped"
                        ),
                    },
                    Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                    Err(e) if e.kind() == ErrorKind::Interrupted => {}
                    Err(error) => {
                        tracing::warn!(
                            target: "ternvale::virtio::vsock",
                            port,
                            path = %path.display(),
                            error = %error,
                            "vsock host accept failed"
                        );
                        break;
                    }
                }
            }
        }
        out
    }
}

impl Drop for HostSide {
    fn drop(&mut self) {
        for (port, _, path) in &self.listeners {
            if let Err(error) = std::fs::remove_file(path) {
                tracing::warn!(
                    target: "ternvale::virtio::vsock",
                    port,
                    path = %path.display(),
                    error = %error,
                    "could not remove vsock listener socket"
                );
            }
        }
    }
}

/// A socket file left by an earlier run would make `bind` fail. Anything else
/// at that path is an error, not something to delete.
fn remove_stale_socket(path: &Path) -> Result<(), VsockError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_socket() => {
            tracing::debug!(
                target: "ternvale::virtio::vsock",
                path = %path.display(),
                "removing stale vsock socket"
            );
            std::fs::remove_file(path).map_err(|source| uds_error(path, source))
        }
        Ok(_) => Err(uds_error(
            path,
            std::io::Error::new(ErrorKind::AlreadyExists, "path exists and is not a socket"),
        )),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(source) => Err(uds_error(path, source)),
    }
}

fn uds_error(path: &Path, source: std::io::Error) -> VsockError {
    tracing::error!(
        target: "ternvale::virtio::vsock",
        path = %path.display(),
        error = %source,
        "vsock unix socket setup failed"
    );
    VsockError::Uds {
        path: path.to_path_buf(),
        source,
    }
}
