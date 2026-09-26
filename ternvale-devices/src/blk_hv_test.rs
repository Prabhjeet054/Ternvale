//! Boot Linux with virtio-blk and write 16 MiB with `dd` into `/dev/vda`.

use std::io::{Read, Seek, SeekFrom, Write};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_vmm::{ExitReason, Machine};

use crate::{Pl011, VirtioBlk};

const WRITE_BYTES: u64 = 16 * 1024 * 1024;

#[test]
#[ignore = "needs-hv"]
fn linux_dd_writes_sixteen_mib_to_virtio_blk() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-assets/virtio-blk");
    assert!(
        root.join("Image").is_file() && root.join("initramfs.cpio").is_file(),
        "missing {} (Alpine 6.6.142 Image plus initramfs that loads virtio_mmio and virtio_blk)",
        root.display()
    );

    let dir = std::env::temp_dir().join(format!("ternvale-blk-{}-linux", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let serial_log = dir.join("guest-serial.log");
    let image = dir.join("disk.img");
    // 512 MiB sparse raw image.
    {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&image)
            .expect("image");
        file.set_len(512 * 1024 * 1024).expect("truncate 512MiB");
    }

    let previous = std::env::var("TERNVALE_LOG").ok();
    // SAFETY: this ignored test sets TERNVALE_LOG and restores it before returning.
    unsafe {
        std::env::set_var(
            "TERNVALE_LOG",
            "info,ternvale::virtio::blk=trace,ternvale::virtio::mmio=info",
        );
    }
    let mut config = ternvale_log::LogConfig::new("virtio-blk", dir.clone());
    config.level = "info,ternvale::virtio::blk=trace".to_string();
    let guard = ternvale_log::init(config).expect("log");

    let vm = VmConfig {
        name: "virtio-blk".to_string(),
        cpus: 1,
        ram_mib: 256,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: String::new(),
        disks: vec![ternvale_config::Disk {
            path: image.clone(),
            read_only: false,
        }],
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: None,
    };
    let uart = Pl011::open(&serial_log).expect("uart");

    let mut ends = [0; 2];
    // SAFETY: `pipe` writes two open fds into `ends` on success.
    let rc = unsafe { libc::pipe(ends.as_mut_ptr()) };
    assert_eq!(rc, 0, "pipe");
    // SAFETY: save stdin, then point it at the pipe read end for Machine.
    let saved = unsafe { libc::dup(libc::STDIN_FILENO) };
    assert!(saved >= 0, "dup stdin");
    let rc = unsafe { libc::dup2(ends[0], libc::STDIN_FILENO) };
    assert_eq!(rc, libc::STDIN_FILENO, "dup2 stdin");
    unsafe { libc::close(ends[0]) };

    let cancel = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let feeder = std::thread::spawn(move || {
        // SAFETY: `ends[1]` is the pipe write end this thread owns.
        let mut input = unsafe { std::fs::File::from_raw_fd(ends[1]) };
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut cursor_replies = 0usize;
        let mut last_reply: Option<Instant> = None;
        let mut wrote = false;
        let mut powered = false;
        while Instant::now() < deadline && !flag.load(Ordering::Acquire) {
            let text = std::fs::read_to_string(&serial_for_feed).unwrap_or_default();
            let queries = text.matches("\u{1b}[6n").count();
            if cursor_replies < queries
                && last_reply.is_none_or(|at| at.elapsed() >= Duration::from_millis(200))
            {
                input.write_all(b"\x1b[24;80R").expect("cursor");
                cursor_replies += 1;
                last_reply = Some(Instant::now());
                std::thread::sleep(Duration::from_millis(50));
                continue;
            }
            if !wrote && text.contains("vda ready") && text.contains("# ") {
                std::thread::sleep(Duration::from_millis(200));
                // PL011 RX FIFO is 16 bytes; send the command in chunks.
                for chunk in [
                    b"dd if=/dev/urand".as_slice(),
                    b"om of=/dev/vda ".as_slice(),
                    b"bs=1M count=16 ".as_slice(),
                    b"&& sync\n".as_slice(),
                ] {
                    input.write_all(chunk).expect("dd chunk");
                    std::thread::sleep(Duration::from_millis(40));
                }
                wrote = true;
            }
            if wrote && !powered && (text.contains("records out") || text.contains("16+0 records"))
            {
                std::thread::sleep(Duration::from_millis(300));
                input.write_all(b"/bin/busybox ").expect("poweroff");
                std::thread::sleep(Duration::from_millis(100));
                input.write_all(b"poweroff -f\n").expect("poweroff");
                powered = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !powered {
            flag.store(true, Ordering::Release);
        }
    });

    let stats_slot = Arc::new(std::sync::Mutex::new(None));
    let stats_for_attach = Arc::clone(&stats_slot);
    let image_for_attach = image.clone();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let (base, size, device, stats) = VirtioBlk::attach(
            0,
            &image_for_attach,
            false,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(0),
        )
        .map_err(|error| ternvale_vmm::MachineError::Attach(error.to_string()))?;
        *stats_for_attach.lock().expect("stats") = Some(stats);
        Ok(vec![(base, size, device)])
    });
    cancel.store(true, Ordering::Release);
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
        serial.contains("vda ready"),
        "guest never saw /dev/vda\n{serial}"
    );
    assert!(
        serial.contains("records out") || serial.contains("16+0 records out"),
        "guest dd did not finish\n{serial}"
    );

    let stats = stats_slot
        .lock()
        .expect("stats")
        .as_ref()
        .expect("attached")
        .snapshot();
    let (reqs, bytes, errors) = stats;
    assert_eq!(
        errors, 0,
        "blk errors={errors} reqs={reqs} bytes={bytes}\n{host}"
    );
    assert!(
        bytes >= WRITE_BYTES,
        "stats bytes {bytes} < {WRITE_BYTES} written by dd; reqs={reqs}\n{host}"
    );
    assert!(reqs > 0, "no blk requests recorded\n{host}");

    let mut file = std::fs::File::open(&image).expect("open image");
    let mut sample = [0u8; 4096];
    let mut nonzero = 0u64;
    let mut offset = 0u64;
    while offset < WRITE_BYTES {
        file.seek(SeekFrom::Start(offset)).expect("seek");
        let n = file.read(&mut sample).expect("read");
        assert!(n > 0, "short read at {offset:#x}");
        nonzero += sample[..n].iter().filter(|&&b| b != 0).count() as u64;
        offset += 1024 * 1024;
    }
    assert!(
        nonzero > 0,
        "first 16 MiB of the image is still all zeros; stats=({reqs},{bytes},{errors})"
    );

    eprintln!(
        "virtio-blk dd ok: stats reqs={reqs} bytes={bytes} errors={errors} nonzero_samples={nonzero}"
    );
    eprintln!("--- guest serial (tail) ---\n{}", last_lines(&serial, 40));
    eprintln!("--- host log (virtio-blk lines) ---");
    for line in host
        .lines()
        .filter(|l| l.contains("virtio-blk") || l.contains("virtqueue"))
    {
        eprintln!("{line}");
    }

    // SAFETY: restore the env var this test changed.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TERNVALE_LOG", value),
            None => std::env::remove_var("TERNVALE_LOG"),
        }
    }
}

fn last_lines(text: &str, n: usize) -> String {
    let lines: Vec<&str> = text.lines().collect();
    let start = lines.len().saturating_sub(n);
    lines[start..].join("\n")
}
