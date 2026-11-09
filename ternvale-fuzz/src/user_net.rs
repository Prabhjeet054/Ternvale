//! User-mode NAT Ethernet/IPv4 path (`UserNet::send`).

use ternvale_net::UserNet;

use crate::quiet;

/// Inject a guest TX frame into the in-process NAT.
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let Ok(mut net) = UserNet::new(Vec::new()) else {
        return;
    };
    let frame = &data[..data.len().min(2048)];
    if let Err(error) = net.send(frame) {
        tracing::debug!(target: "ternvale::fuzz", error = %error, "user-net send");
    }
    let _rx = net.poll();
}
