//! Control-socket peer checks (`LOCAL_PEERCRED`).

use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;

use anyhow::{bail, Context, Result};

/// Reject `peer` when it is not `host` (same rule as [`authorize`]).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::api",
    skip_all,
    fields(peer, host)
)]
pub fn authorize_peer_uid(peer: u32, host: u32) -> Result<()> {
    if peer != host {
        tracing::warn!(
            target: "ternvale::api",
            peer_uid = peer,
            host_uid = host,
            "rejected control connection: peer UID mismatch"
        );
        bail!("control socket peer uid {peer} is not this user ({host})");
    }
    tracing::debug!(
        target: "ternvale::api",
        uid = peer,
        "control peer uid matches"
    );
    Ok(())
}

/// Reject a connection whose peer UID is not this process's UID.
#[tracing::instrument(level = "debug", target = "ternvale::api", skip_all)]
pub fn authorize(stream: &UnixStream) -> Result<()> {
    let fd = stream.as_raw_fd();
    let peer = ternvale_hv::unix_peer_uid(fd).context("LOCAL_PEERCRED")?;
    authorize_peer_uid(peer, ternvale_hv::host_uid())
}

#[cfg(test)]
mod tests {
    use super::{authorize, authorize_peer_uid};
    use std::os::unix::net::UnixStream;

    #[test]
    fn same_user_pair_is_authorized() {
        let (a, b) = UnixStream::pair().expect("pair");
        authorize(&a).expect("a");
        authorize(&b).expect("b");
    }

    #[test]
    fn simulated_wrong_uid_is_rejected() {
        authorize_peer_uid(501, 501).expect("same");
        let err = authorize_peer_uid(502, 501).expect_err("other user");
        assert!(err.to_string().contains("peer uid 502"), "{err:#}");
    }
}
