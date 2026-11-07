//! FUSE opcodes (Linux `fuse.h` / virtio-fs). Logged at TRACE per request.

/// `FUSE_LOOKUP`.
pub const LOOKUP: u32 = 1;
/// `FUSE_FORGET` (no reply).
pub const FORGET: u32 = 2;
/// `FUSE_GETATTR`.
pub const GETATTR: u32 = 3;
/// `FUSE_SETATTR`.
pub const SETATTR: u32 = 4;
/// `FUSE_READLINK`.
pub const READLINK: u32 = 5;
/// `FUSE_SYMLINK`.
pub const SYMLINK: u32 = 6;
/// `FUSE_MKDIR`.
pub const MKDIR: u32 = 9;
/// `FUSE_UNLINK`.
pub const UNLINK: u32 = 10;
/// `FUSE_RMDIR`.
pub const RMDIR: u32 = 11;
/// `FUSE_RENAME`.
pub const RENAME: u32 = 12;
/// `FUSE_OPEN`.
pub const OPEN: u32 = 14;
/// `FUSE_READ`.
pub const READ: u32 = 15;
/// `FUSE_WRITE`.
pub const WRITE: u32 = 16;
/// `FUSE_STATFS`.
pub const STATFS: u32 = 17;
/// `FUSE_RELEASE`.
pub const RELEASE: u32 = 18;
/// `FUSE_FSYNC`.
pub const FSYNC: u32 = 20;
/// `FUSE_SETXATTR`.
pub const SETXATTR: u32 = 21;
/// `FUSE_GETXATTR`.
pub const GETXATTR: u32 = 22;
/// `FUSE_LISTXATTR`.
pub const LISTXATTR: u32 = 23;
/// `FUSE_REMOVEXATTR`.
pub const REMOVEXATTR: u32 = 24;
/// `FUSE_FLUSH`.
pub const FLUSH: u32 = 25;
/// `FUSE_INIT`.
pub const INIT: u32 = 26;
/// `FUSE_OPENDIR`.
pub const OPENDIR: u32 = 27;
/// `FUSE_READDIR`.
pub const READDIR: u32 = 28;
/// `FUSE_RELEASEDIR`.
pub const RELEASEDIR: u32 = 29;
/// `FUSE_FSYNCDIR`.
pub const FSYNCDIR: u32 = 30;
/// `FUSE_ACCESS`.
pub const ACCESS: u32 = 34;
/// `FUSE_CREATE`.
pub const CREATE: u32 = 35;
/// `FUSE_READDIRPLUS`.
pub const READDIRPLUS: u32 = 44;
/// `FUSE_RENAME2`.
pub const RENAME2: u32 = 45;

/// Stable name for logs.
#[tracing::instrument(
    level = "trace",
    target = "ternvale::virtiofs",
    skip_all,
    fields(opcode)
)]
pub fn name(opcode: u32) -> &'static str {
    match opcode {
        LOOKUP => "LOOKUP",
        FORGET => "FORGET",
        GETATTR => "GETATTR",
        SETATTR => "SETATTR",
        READLINK => "READLINK",
        SYMLINK => "SYMLINK",
        MKDIR => "MKDIR",
        UNLINK => "UNLINK",
        RMDIR => "RMDIR",
        RENAME => "RENAME",
        OPEN => "OPEN",
        READ => "READ",
        WRITE => "WRITE",
        STATFS => "STATFS",
        RELEASE => "RELEASE",
        FSYNC => "FSYNC",
        SETXATTR => "SETXATTR",
        GETXATTR => "GETXATTR",
        LISTXATTR => "LISTXATTR",
        REMOVEXATTR => "REMOVEXATTR",
        FLUSH => "FLUSH",
        INIT => "INIT",
        OPENDIR => "OPENDIR",
        READDIR => "READDIR",
        RELEASEDIR => "RELEASEDIR",
        FSYNCDIR => "FSYNCDIR",
        ACCESS => "ACCESS",
        CREATE => "CREATE",
        READDIRPLUS => "READDIRPLUS",
        RENAME2 => "RENAME2",
        _ => "UNKNOWN",
    }
}

#[cfg(test)]
mod tests {
    use super::{name, INIT, LOOKUP};

    #[test]
    fn known_opcodes_have_names() {
        assert_eq!(name(INIT), "INIT");
        assert_eq!(name(LOOKUP), "LOOKUP");
        assert_eq!(name(99), "UNKNOWN");
    }
}
