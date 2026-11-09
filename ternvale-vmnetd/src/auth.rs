//! Codesign requirement check on each helper client.

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use ternvale_sandbox::authorize_helper_client;

/// Reject unsigned or wrong-identity callers.
#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
pub fn authorize(stream: &UnixStream) -> std::io::Result<()> {
    authorize_helper_client(stream.as_raw_fd()).map_err(|error| {
        tracing::warn!(
            target: "ternvale::net::vmnet",
            error = %error,
            "rejected privileged helper client"
        );
        std::io::Error::new(std::io::ErrorKind::PermissionDenied, error.to_string())
    })
}
