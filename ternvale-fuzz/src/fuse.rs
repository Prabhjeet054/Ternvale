//! FUSE in-header parser and in-process virtio-fs server.

use ternvale_virtiofs::{InHeader, Options, Server};

use crate::guest::scratch;
use crate::quiet;

/// Parse the header, then dispatch through [`Server::handle`].
#[tracing::instrument(level = "debug", target = "ternvale::fuzz", skip_all, fields(len = data.len()))]
pub fn run(data: &[u8]) {
    quiet();
    let _ = InHeader::parse(data);
    let dir = scratch("fuse");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let opened = Server::open(
        &dir,
        Options {
            read_only: false,
            max_handles: 8,
            windows_names: data.first().copied().unwrap_or(0) & 1 != 0,
        },
    );
    if let Ok(mut server) = opened {
        let _ = server.handle(&data[..data.len().min(4096)]);
    }
    crate::guest::cleanup(&dir);
}
