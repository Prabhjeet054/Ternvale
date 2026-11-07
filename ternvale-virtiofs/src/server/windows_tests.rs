//! Windows share behavior exercised through the FUSE server (no guest VM).
//! Live Windows 11 drive-letter checks go through
//! `scripts/windows-virtiofs-live-check.sh`.

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

fn init(server: &mut Server) {
    let mut req = header(opcode::INIT, 1, 1);
    req.extend_from_slice(&7u32.to_le_bytes());
    req.extend_from_slice(&31u32.to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes());
    let reply = server.handle(&req).expect("init");
    assert_eq!(unique_error(&reply), 0);
}

fn win_share(label: &str) -> (std::path::PathBuf, Server) {
    let dir = std::env::temp_dir().join(format!("tv-fs-wtest-{label}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("share");
    let server = Server::open(
        &dir,
        Options {
            windows_names: true,
            ..Options::default()
        },
    )
    .expect("open");
    (dir, server)
}

fn lookup(server: &mut Server, unique: u64, name: &str) -> (i32, u64) {
    let mut req = header(opcode::LOOKUP, unique, fuse::ROOT_ID);
    req.extend_from_slice(name.as_bytes());
    req.push(0);
    let reply = server.handle(&req).expect("lookup");
    let err = unique_error(&reply);
    let nodeid = if err == 0 {
        u64::from_le_bytes(
            reply
                .get(16..24)
                .and_then(|s| s.try_into().ok())
                .unwrap_or([0; 8]),
        )
    } else {
        0
    };
    (err, nodeid)
}

fn mkdir_named(server: &mut Server, unique: u64, name: &str) -> i32 {
    let mut req = header(opcode::MKDIR, unique, fuse::ROOT_ID);
    req.extend_from_slice(&0o755u32.to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes());
    req.extend_from_slice(name.as_bytes());
    req.push(0);
    unique_error(&server.handle(&req).expect("mkdir"))
}

fn create_named(server: &mut Server, unique: u64, parent: u64, name: &str) -> (i32, u64, u64) {
    let mut req = header(opcode::CREATE, unique, parent);
    req.extend_from_slice(&0u32.to_le_bytes());
    req.extend_from_slice(&0o644u32.to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes());
    req.extend_from_slice(name.as_bytes());
    req.push(0);
    let reply = server.handle(&req).expect("create");
    let err = unique_error(&reply);
    if err != 0 {
        return (err, 0, 0);
    }
    let nodeid = u64::from_le_bytes(
        reply
            .get(16..24)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 8]),
    );
    let fh = u64::from_le_bytes(
        reply
            .get(16 + fuse::ENTRY_OUT..16 + fuse::ENTRY_OUT + 8)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 8]),
    );
    (err, nodeid, fh)
}

fn write_all(server: &mut Server, unique: u64, nodeid: u64, fh: u64, data: &[u8]) -> i32 {
    let mut req = header(opcode::WRITE, unique, nodeid);
    req.extend_from_slice(&fh.to_le_bytes());
    req.extend_from_slice(&0u64.to_le_bytes());
    req.extend_from_slice(&(data.len() as u32).to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes());
    req.extend_from_slice(&[0u8; 16]);
    req.extend_from_slice(data);
    unique_error(&server.handle(&req).expect("write"))
}

fn read_all(server: &mut Server, unique: u64, nodeid: u64, fh: u64, size: usize) -> Vec<u8> {
    let mut req = header(opcode::READ, unique, nodeid);
    req.extend_from_slice(&fh.to_le_bytes());
    req.extend_from_slice(&0u64.to_le_bytes());
    req.extend_from_slice(&(size as u32).to_le_bytes());
    req.extend_from_slice(&0u32.to_le_bytes());
    let reply = server.handle(&req).expect("read");
    assert_eq!(unique_error(&reply), 0);
    reply.get(16..).unwrap_or(&[]).to_vec()
}

fn unlink_named(server: &mut Server, unique: u64, name: &str) -> i32 {
    let mut req = header(opcode::UNLINK, unique, fuse::ROOT_ID);
    req.extend_from_slice(name.as_bytes());
    req.push(0);
    unique_error(&server.handle(&req).expect("unlink"))
}

#[test]
fn windows_both_sides_create_edit_delete() {
    let (dir, mut server) = win_share("both");
    init(&mut server);

    std::fs::write(dir.join("from_host.txt"), b"host-v1").expect("host write");
    let (err, nodeid) = lookup(&mut server, 2, "from_host.txt");
    assert_eq!(err, 0, "guest LOOKUP must see a host-created file promptly");
    let mut open = header(opcode::OPEN, 3, nodeid);
    open.extend_from_slice(&0u32.to_le_bytes());
    open.extend_from_slice(&0u32.to_le_bytes());
    let open_reply = server.handle(&open).expect("open");
    assert_eq!(unique_error(&open_reply), 0);
    let fh = u64::from_le_bytes(
        open_reply
            .get(16..24)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 8]),
    );
    assert_eq!(read_all(&mut server, 4, nodeid, fh, 32), b"host-v1");
    std::fs::write(dir.join("from_host.txt"), b"host-v2").expect("host edit");
    assert_eq!(read_all(&mut server, 5, nodeid, fh, 32), b"host-v2");

    let (err, _, fh2) = create_named(&mut server, 6, fuse::ROOT_ID, "from_guest.txt");
    assert_eq!(err, 0);
    assert_eq!(
        write_all(&mut server, 7, fuse::ROOT_ID, fh2, b"guest-v1"),
        0
    );
    assert_eq!(
        std::fs::read(dir.join("from_guest.txt")).expect("host read guest file"),
        b"guest-v1"
    );
    assert_eq!(
        write_all(&mut server, 8, fuse::ROOT_ID, fh2, b"guest-v2"),
        0
    );
    assert_eq!(
        std::fs::read(dir.join("from_guest.txt")).expect("host read edit"),
        b"guest-v2"
    );

    std::fs::remove_file(dir.join("from_host.txt")).expect("host delete");
    let (err, _) = lookup(&mut server, 9, "from_host.txt");
    assert_eq!(err, -libc::ENOENT);
    assert_eq!(unlink_named(&mut server, 10, "from_guest.txt"), 0);
    assert!(!dir.join("from_guest.txt").exists());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn windows_names_spaces_unicode_long_reserved() {
    let (dir, mut server) = win_share("names");
    init(&mut server);

    assert_eq!(
        create_named(&mut server, 2, fuse::ROOT_ID, "my file.txt").0,
        0
    );
    assert!(dir.join("my file.txt").is_file());

    assert_eq!(
        create_named(&mut server, 3, fuse::ROOT_ID, "café 世界.txt").0,
        0
    );
    assert!(dir.join("café 世界.txt").is_file());

    let long = "n".repeat(200);
    assert_eq!(create_named(&mut server, 4, fuse::ROOT_ID, &long).0, 0);
    assert!(dir.join(&long).is_file());

    assert_eq!(mkdir_named(&mut server, 5, "CON"), -libc::EINVAL);
    assert_eq!(
        create_named(&mut server, 6, fuse::ROOT_ID, "aux.txt").0,
        -libc::EINVAL
    );
    assert!(!dir.join("CON").exists());
    assert!(!dir.join("aux.txt").exists());

    let deep = "d".repeat(100);
    assert_eq!(mkdir_named(&mut server, 7, &deep), 0);
    let (err, parent) = lookup(&mut server, 8, &deep);
    assert_eq!(err, 0);
    let too_long = "f".repeat(160);
    let (cerr, _, _) = create_named(&mut server, 9, parent, &too_long);
    assert_eq!(
        cerr,
        -libc::ENAMETOOLONG,
        "Z:\\ + 100 + \\ + 160 exceeds Windows MAX_PATH 260"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

fn shasum(path: &std::path::Path) -> String {
    let out = std::process::Command::new("shasum")
        .args(["-a", "256"])
        .arg(path)
        .output()
        .expect("shasum");
    assert!(out.status.success(), "shasum {}", path.display());
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_string()
}

/// 1 GiB host→FUSE and FUSE→host, SHA-256 both ways. Invoked by the live
/// check script (`--ignored`), not every `cargo test --workspace`.
#[test]
#[ignore = "1g"]
fn windows_1g_copy_both_ways() {
    const GIB: u64 = 1 << 30;
    const CHUNK: usize = 1024 * 1024;
    let (dir, mut server) = win_share("1g");
    init(&mut server);

    let host_to_guest = dir.join("host_to_guest.bin");
    {
        let mut file = std::fs::File::create(&host_to_guest).expect("create 1g");
        let mut block = vec![0x5au8; CHUNK];
        let mut left = GIB;
        while left > 0 {
            let n = CHUNK.min(left as usize);
            std::io::Write::write_all(&mut file, &block[..n]).expect("write 1g");
            block[0] = block[0].wrapping_add(1);
            left -= n as u64;
        }
    }
    let host_sum = shasum(&host_to_guest);
    let (err, nodeid) = lookup(&mut server, 2, "host_to_guest.bin");
    assert_eq!(err, 0);
    let mut open = header(opcode::OPEN, 3, nodeid);
    open.extend_from_slice(&0u32.to_le_bytes());
    open.extend_from_slice(&0u32.to_le_bytes());
    let open_reply = server.handle(&open).expect("open 1g");
    assert_eq!(unique_error(&open_reply), 0);
    let fh = u64::from_le_bytes(
        open_reply
            .get(16..24)
            .and_then(|s| s.try_into().ok())
            .unwrap_or([0; 8]),
    );
    let guest_copy = dir.join("guest_read.bin");
    {
        let mut out = std::fs::File::create(&guest_copy).expect("guest copy");
        let mut offset = 0u64;
        while offset < GIB {
            let mut req = header(opcode::READ, 4 + offset / CHUNK as u64, nodeid);
            req.extend_from_slice(&fh.to_le_bytes());
            req.extend_from_slice(&offset.to_le_bytes());
            req.extend_from_slice(&(CHUNK as u32).to_le_bytes());
            req.extend_from_slice(&0u32.to_le_bytes());
            let reply = server.handle(&req).expect("read chunk");
            assert_eq!(unique_error(&reply), 0);
            let payload = reply.get(16..).unwrap_or(&[]);
            std::io::Write::write_all(&mut out, payload).expect("copy chunk");
            offset += payload.len() as u64;
        }
    }
    assert_eq!(shasum(&guest_copy), host_sum, "host→guest 1 GiB SHA-256");

    let (err, nodeid, fh) = create_named(&mut server, 20, fuse::ROOT_ID, "guest_to_host.bin");
    assert_eq!(err, 0);
    let mut block = vec![0xa5u8; CHUNK];
    let mut offset = 0u64;
    while offset < GIB {
        let mut req = header(opcode::WRITE, 21 + offset / CHUNK as u64, nodeid);
        req.extend_from_slice(&fh.to_le_bytes());
        req.extend_from_slice(&offset.to_le_bytes());
        req.extend_from_slice(&(CHUNK as u32).to_le_bytes());
        req.extend_from_slice(&0u32.to_le_bytes());
        req.extend_from_slice(&[0u8; 16]);
        req.extend_from_slice(&block);
        let reply = server.handle(&req).expect("write chunk");
        assert_eq!(unique_error(&reply), 0);
        block[0] = block[0].wrapping_add(1);
        offset += CHUNK as u64;
    }
    let guest_sum = shasum(&dir.join("guest_to_host.bin"));
    let host_again = dir.join("host_read.bin");
    std::fs::copy(dir.join("guest_to_host.bin"), &host_again).expect("host copy");
    assert_eq!(shasum(&host_again), guest_sum, "guest→host 1 GiB SHA-256");
    tracing::info!(
        target: "ternvale::virtiofs",
        host_to_guest = %host_sum,
        guest_to_host = %guest_sum,
        "1 GiB Windows-share checksums match both ways"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
