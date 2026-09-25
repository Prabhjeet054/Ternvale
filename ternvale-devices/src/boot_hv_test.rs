//! Boot the Alpine test kernel and busybox initramfs through `Machine`.

use std::io::Write;
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_vmm::{ExitReason, Machine};

use crate::Pl011;

#[test]
#[ignore = "needs-hv"]
fn boots_to_a_busybox_prompt_and_echoes_hello() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-assets");
    let dir = std::env::temp_dir().join(format!("ternvale-boot-{}-linux", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let serial_log = dir.join("guest-serial.log");
    let previous = std::env::var("TERNVALE_LOG").ok();
    // SAFETY: this ignored test sets TERNVALE_LOG and restores it before returning.
    unsafe { std::env::set_var("TERNVALE_LOG", "info") };
    let mut config = ternvale_log::LogConfig::new("linux", dir.clone());
    config.level = "info".to_string();
    let guard = ternvale_log::init(config).expect("log");

    let vm = VmConfig {
        name: "linux".to_string(),
        cpus: 1,
        ram_mib: 256,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: String::new(),
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: None,
    };
    let uart = Pl011::open(&serial_log).expect("uart");

    let mut ends = [0; 2];
    // SAFETY: `pipe` writes two open fds into `ends` on success.
    let rc = unsafe { libc::pipe(ends.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe");
    // SAFETY: `dup` saves stdin so the test can restore it. `dup2` makes the
    // pipe the process stdin that `Machine` reads.
    let saved = unsafe { libc::dup(libc::STDIN_FILENO) };
    assert!(saved >= 0, "dup stdin");
    let rc = unsafe { libc::dup2(ends[0], libc::STDIN_FILENO) };
    assert_eq!(rc, libc::STDIN_FILENO, "dup2 stdin");
    unsafe { libc::close(ends[0]) };

    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let serial_for_feed = serial_log.clone();
    let feeder = std::thread::spawn(move || {
        // SAFETY: `ends[1]` is the pipe write end this thread owns.
        let mut input = unsafe { std::fs::File::from_raw_fd(ends[1]) };
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut sent_echo = false;
        let mut cursor_replies = 0usize;
        let mut last_reply: Option<Instant> = None;
        while Instant::now() < deadline && !flag.load(Ordering::Acquire) {
            let text = std::fs::read_to_string(&serial_for_feed).unwrap_or_default();
            let queries = text.matches("\u{1b}[6n").count();
            // BusyBox blocks in the cursor-position read until it sees 'R'.
            // A later line written during that read is consumed as the reply.
            let settled = last_reply.is_some_and(|at| at.elapsed() >= Duration::from_millis(200));
            if cursor_replies < queries
                && last_reply.is_none_or(|at| at.elapsed() >= Duration::from_millis(200))
            {
                input.write_all(b"\x1b[24;80R").expect("write cursor");
                cursor_replies += 1;
                last_reply = Some(Instant::now());
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            if !sent_echo && settled && (text.contains("# ") || text.contains("BusyBox")) {
                input.write_all(b"echo hello\n").expect("write echo");
                sent_echo = true;
            }
            if sent_echo && settled && cursor_replies == queries && text.contains("hello") {
                // The PL011 RX FIFO holds 16 bytes. The command is longer than that.
                input.write_all(b"/bin/busybox ").expect("write poweroff");
                std::thread::sleep(Duration::from_millis(100));
                input.write_all(b"poweroff -f\n").expect("write poweroff");
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    });

    let exit = Machine::run(&vm, Box::new(uart));
    stop.store(true, Ordering::Release);
    feeder.join().expect("feeder");
    // SAFETY: restore the stdin saved before the pipe was installed.
    unsafe {
        libc::dup2(saved, libc::STDIN_FILENO);
        libc::close(saved);
    }

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let host = std::fs::read_to_string(&path).unwrap_or_default();
    assert!(
        exit.as_ref().ok() == Some(&ExitReason::SystemOff),
        "exit={exit:?}\n--- serial ---\n{serial}\n--- host ---\n{host}"
    );
    assert!(
        serial.contains("Linux version"),
        "missing kernel banner\n{serial}"
    );
    assert!(
        serial.contains("BusyBox") || serial.contains("# "),
        "missing busybox prompt\n{serial}"
    );
    assert!(serial.contains("hello"), "missing echo hello\n{serial}");

    // SAFETY: same as the set above; this test restores the variable it changed.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TERNVALE_LOG", value),
            None => std::env::remove_var("TERNVALE_LOG"),
        }
    }
}
