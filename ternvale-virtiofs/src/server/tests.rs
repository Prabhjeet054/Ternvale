use crate::fuse::{self, IN_HEADER};
use crate::opcode;
use crate::server::{Options, Server};

fn header(opcode: u32, unique: u64, nodeid: u64) -> Vec<u8> {
    let mut buf = vec![0u8; IN_HEADER];
    buf[0..4].copy_from_slice(&(IN_HEADER as u32).to_le_bytes());
    buf[4..8].copy_from_slice(&opcode.to_le_bytes());
    buf[8..16].copy_from_slice(&unique.to_le_bytes());
    buf[16..24].copy_from_slice(&nodeid.to_le_bytes());
    buf
}

fn unique_error(reply: &[u8]) -> i32 {
    i32::from_le_bytes(
        reply
            .get(4..8)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 4]),
    )
}

fn tmp_share(label: &str) -> (std::path::PathBuf, Server) {
    let dir = std::env::temp_dir().join(format!(
        "tv-fs-{label}-{}-{}",
        std::process::id(),
        label.len()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("share");
    let server = Server::open(&dir, Options::default()).expect("open");
    (dir, server)
}

fn init(server: &mut Server) {
    let mut req = header(opcode::INIT, 1, 1);
    req.extend_from_slice(&7u32.to_le_bytes());
    req.extend_from_slice(&31u32.to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes());
    let reply = server.handle(&req).expect("init reply");
    assert_eq!(unique_error(&reply), 0);
}

#[test]
fn lookup_rejects_dotdot() {
    let (dir, mut server) = tmp_share("dotdot");
    init(&mut server);
    let mut req = header(opcode::LOOKUP, 2, fuse::ROOT_ID);
    req.extend_from_slice(b"..\0");
    let reply = server.handle(&req).expect("lookup");
    assert_eq!(unique_error(&reply), -libc::EPERM);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn write_on_read_only_is_erofs() {
    let dir = std::env::temp_dir().join(format!("tv-fs-ro-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let mut server = Server::open(
        &dir,
        Options {
            read_only: true,
            max_handles: 8,
        },
    )
    .expect("open");
    init(&mut server);
    let mut req = header(opcode::MKDIR, 3, fuse::ROOT_ID);
    req.extend_from_slice(&0o755u32.to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes());
    req.extend_from_slice(b"nope\0");
    let reply = server.handle(&req).expect("mkdir");
    assert_eq!(unique_error(&reply), -libc::EROFS);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn forget_has_no_reply() {
    let (dir, mut server) = tmp_share("forget");
    init(&mut server);
    let mut req = header(opcode::FORGET, 4, fuse::ROOT_ID);
    req.extend_from_slice(&1u64.to_le_bytes());
    assert!(server.handle(&req).is_none());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn escaping_host_symlink_is_not_followed() {
    let (dir, mut server) = tmp_share("escape");
    let outside = std::env::temp_dir().join(format!("tv-fs-out-{}", std::process::id()));
    std::fs::write(&outside, b"secret").expect("outside");
    std::os::unix::fs::symlink(&outside, dir.join("link")).expect("symlink");
    init(&mut server);
    let mut req = header(opcode::LOOKUP, 5, fuse::ROOT_ID);
    req.extend_from_slice(b"link\0");
    let reply = server.handle(&req).expect("lookup");
    assert_eq!(unique_error(&reply), 0);
    // Nodeid is the first 8 bytes of fuse_entry_out after the out header.
    let nodeid = u64::from_le_bytes(
        reply
            .get(16..24)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 8]),
    );
    let open = header(opcode::OPEN, 6, nodeid);
    let open_reply = server.handle(&open).expect("open");
    // Opening the symlink inode must not yield the outside file's bytes.
    if unique_error(&open_reply) == 0 {
        let fh = u64::from_le_bytes(
            open_reply
                .get(16..24)
                .and_then(|s| s.try_into().ok())
                .unwrap_or([0; 8]),
        );
        let mut read = header(opcode::READ, 7, nodeid);
        read.extend_from_slice(&fh.to_le_bytes());
        read.extend_from_slice(&0u64.to_le_bytes());
        read.extend_from_slice(&64u32.to_le_bytes());
        read.extend_from_slice(&0u32.to_le_bytes());
        let read_reply = server.handle(&read).expect("read");
        let payload = read_reply.get(16..).unwrap_or(&[]);
        assert_ne!(payload, b"secret");
    }
    let _ = std::fs::remove_file(&outside);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn getattr_root_maps_host_ids() {
    let (dir, mut server) = tmp_share("attr");
    init(&mut server);
    let req = header(opcode::GETATTR, 8, fuse::ROOT_ID);
    let reply = server.handle(&req).expect("getattr");
    assert_eq!(unique_error(&reply), 0);
    let uid = u32::from_le_bytes(
        reply
            .get(16 + 16 + 68..16 + 16 + 72)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 4]),
    );
    let (host_uid, _) = crate::sys::host_ids();
    assert_eq!(uid, host_uid);
    let _ = std::fs::remove_dir_all(&dir);
}
