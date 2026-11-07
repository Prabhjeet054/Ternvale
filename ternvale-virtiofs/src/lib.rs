//! In-process virtio-fs (FUSE) server and virtio-mmio device.
//!
//! Paths are resolved with `openat` / `O_NOFOLLOW` from the share root. Guest
//! `..` names, escaping symlinks, and writes on a read-only export are rejected
//! at WARN. FUSE opcodes are logged at TRACE (`ternvale::virtiofs`).

mod device;
mod error;
mod fuse;
mod handle;
mod inode;
mod name;
mod opcode;
mod server;
mod sys;
mod wire;
mod xattr;

pub use device::{AttachedFs, FsStats, VirtioFs, VIRTIO_FS_ID, VIRTIO_FS_TAG_LEN};
pub use error::FsError;
pub use server::{Options, Server};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-virtiofs");
    }
}
