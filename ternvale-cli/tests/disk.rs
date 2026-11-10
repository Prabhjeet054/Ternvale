//! Step 86: fstrim / online grow / compact checksums / refused shrink.

mod support;

use std::io::{Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use support::{CliOut, Harness, Vm};
use ternvale_snapshot::{ImageStack, DEFAULT_BLOCK_SIZE};

const T: &str = "ternvale::cli";
const BOOT: Duration = Duration::from_secs(120);
const BLOCK: u64 = DEFAULT_BLOCK_SIZE as u64;
const GIB: u64 = 1 << 30;
const SECTORS_8G: &str = "16777216";
const SECTORS_16G: &str = "33554432";

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
fn du_k(path: &Path) -> u64 {
    let out = Command::new("/usr/bin/du")
        .args(["-k", &path.display().to_string()])
        .output()
        .expect("du -k");
    assert!(
        out.status.success(),
        "du: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let n = String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .expect("du")
        .parse::<u64>()
        .expect("int");
    tracing::info!(target: T, path = %path.display(), kib = n, "du -k");
    n
}

fn logical(path: &Path) -> u64 {
    std::fs::metadata(path).expect("meta").len()
}

fn sha256(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn assert_ok(out: &CliOut, what: &str) {
    assert_eq!(out.code, Some(0), "{what}: {out:?}");
}

fn root_assets() -> Option<PathBuf> {
    let dir = support::root().join("test-assets/virtio-root");
    for name in ["Image", "initramfs.cpio", "rootfs.ext4"] {
        if !dir.join(name).is_file() {
            tracing::warn!(target: T, missing = name, "virtio-root assets missing");
            return None;
        }
    }
    Some(dir)
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name, bytes))]
fn install_vm(h: &Harness, name: &str, bytes: u64) -> PathBuf {
    let dir = h.out.join("vms").join(name);
    std::fs::create_dir_all(dir.join("disks")).expect("vms");
    let disk = dir.join("disks/disk0.img");
    {
        let file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&disk)
            .expect("disk");
        file.set_len(bytes).expect("len");
    }
    let kernel = support::root().join("test-assets/virtio-blk/Image");
    let initrd = support::root().join("test-assets/virtio-blk/initramfs.cpio");
    let text = format!(
        "name = \"{name}\"\ncpus = 1\nram_mib = 256\nkernel = \"{}\"\ninitrd = \"{}\"\nserial_log = \"{}\"\ncmdline = \"console=ttyAMA0\"\n\n[[disks]]\npath = \"{}\"\nread_only = false\n",
        kernel.display(),
        initrd.display(),
        h.out.join(format!("{name}-serial.log")).display(),
        disk.display()
    );
    std::fs::write(dir.join("config.toml"), text).expect("config");
    disk
}

fn type_line(vm: &mut Vm, line: &str) {
    let mut rest = line;
    while !rest.is_empty() {
        let n = rest.len().min(16);
        vm.type_chunks(&[&rest[..n]]);
        rest = &rest[n..];
    }
}

fn wait_root(vm: &mut Vm) {
    vm.wait_serial("guest root", BOOT, |s| {
        s.contains("ternvale root ready") || (s.contains("vda ready") && s.contains("# "))
    });
}

fn wait_socket(h: &Harness) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while Instant::now() < deadline {
        if h.socket().exists() {
            return;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    panic!("no control socket at {}", h.socket().display());
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn write_root_config(h: &Harness, assets: &Path, disk: &Path) -> PathBuf {
    let path = h.out.join("vm.toml");
    let text = format!(
        "name = \"{}\"\ncpus = 1\nram_mib = 512\nkernel = \"{}\"\ninitrd = \"{}\"\nserial_log = \"{}\"\ncmdline = \"console=ttyAMA0\"\n\n[[disks]]\npath = \"{}\"\nread_only = false\n",
        h.name,
        assets.join("Image").display(),
        assets.join("initramfs.cpio").display(),
        h.out.join("guest-serial.log").display(),
        disk.display()
    );
    std::fs::write(&path, text).expect("cfg");
    path
}

fn sparse_root(h: &Harness, assets: &Path, bytes: u64) -> PathBuf {
    let image = h.out.join("root.ext4");
    std::fs::copy(assets.join("rootfs.ext4"), &image).expect("copy rootfs");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(&image)
        .expect("open");
    file.set_len(bytes).expect("grow image");
    prepare_ext4(&image);
    image
}

/// Grow the copied Alpine ext4 to the sparse file size and install
/// `resize2fs` / `fstrim` into the image (host has no e2fsprogs).
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
fn prepare_ext4(path: &Path) {
    let bind = format!("{}:/disk.img", path.display());
    let out = Command::new("docker")
        .args([
            "run",
            "--rm",
            "--privileged",
            "--platform",
            "linux/arm64",
            "-v",
            &bind,
            "alpine:3.20",
            "sh",
            "-c",
            "set -euo pipefail; apk add --no-cache e2fsprogs util-linux >/dev/null; mkdir -p /mnt; mount -o loop /disk.img /mnt; apk add --root /mnt --no-cache --repositories-file /etc/apk/repositories e2fsprogs util-linux >/dev/null; umount /mnt; resize2fs /disk.img",
        ])
        .output()
        .expect("docker resize2fs");
    assert!(
        out.status.success(),
        "prepare ext4: {} {}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    tracing::info!(target: T, path = %path.display(), "ext4 grown and tools installed");
}

#[test]
fn unsafe_shrink_is_refused() {
    let h = Harness::new_unsigned("dskshr");
    let disk = install_vm(&h, "src", 8 * 1024 * 1024);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&disk)
            .expect("open");
        file.write_all(&[0x5a; 4096]).expect("write head");
        file.seek(SeekFrom::Start(7 * 1024 * 1024)).expect("seek");
        file.write_all(&[0xa5; 4096]).expect("write tail");
        file.sync_all().expect("sync");
    }
    let out = h.tv(&["disk", "resize", "src", "1M"]);
    assert_ne!(out.code, Some(0), "{out:?}");
    let msg = format!("{}{}", out.stdout, out.stderr);
    assert!(
        msg.contains("in-use") || msg.contains("refusing"),
        "expected refused shrink: {out:?}"
    );
    assert_eq!(logical(&disk), 8 * 1024 * 1024);
    tracing::info!(target: T, "unsafe shrink refused");
}

#[test]
fn compact_delta_chain_preserves_checksums() {
    let h = Harness::new_unsigned("dskcmp");
    let disk = install_vm(&h, "src", 16 * BLOCK);
    {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .open(&disk)
            .expect("base");
        file.write_all(&[0x11; BLOCK as usize]).expect("base0");
    }
    assert_ok(&h.tv(&["clone", "src", "cow"]), "clone");
    let stack_path = h.out.join("vms/cow/disks/disk0.tstack");
    let payload0 = vec![0xaau8; BLOCK as usize];
    let payload2 = vec![0xccu8; BLOCK as usize];
    let hash0 = sha256(&payload0);
    let hash2 = sha256(&payload2);
    {
        let mut stack = ImageStack::open(&stack_path, false).expect("stack");
        stack.write_at(&payload0, 0).expect("w0");
        stack
            .write_at(&vec![0xbbu8; BLOCK as usize], BLOCK)
            .expect("w1");
        stack.write_at(&payload2, 2 * BLOCK).expect("w2");
        stack.discard(BLOCK, BLOCK, true).expect("unmap middle");
    }
    let delta = h.out.join("vms/cow/disks/disk0.tdelta");
    let before_du = du_k(&delta);
    let before_len = logical(&delta);
    assert_ok(&h.tv(&["disk", "compact", "cow"]), "compact");
    let after_du = du_k(&delta);
    let after_len = logical(&delta);
    tracing::info!(
        target: T,
        before_du,
        after_du,
        before_len,
        after_len,
        "compact allocated"
    );
    assert!(
        after_len < before_len || after_du < before_du,
        "compact should drop unmapped payload (len {before_len}->{after_len}, du {before_du}->{after_du})"
    );
    let mut got0 = vec![0u8; BLOCK as usize];
    let mut got2 = vec![0u8; BLOCK as usize];
    let mut mid = vec![0u8; BLOCK as usize];
    {
        let stack = ImageStack::open(&stack_path, true).expect("reopen");
        stack.read_at(&mut got0, 0).expect("r0");
        stack.read_at(&mut mid, BLOCK).expect("rm");
        stack.read_at(&mut got2, 2 * BLOCK).expect("r2");
    }
    assert_eq!(sha256(&got0), hash0, "block 0 checksum");
    assert_eq!(sha256(&got2), hash2, "block 2 checksum");
    assert_eq!(mid, vec![0u8; BLOCK as usize], "unmapped block is zeros");
}

#[test]
#[ignore = "needs-hv"]
fn hv_fstrim_drops_host_du_logical_unchanged() {
    let Some(assets) = root_assets() else {
        return;
    };
    let h = Harness::new("dsktrim");
    let image = sparse_root(&h, &assets, 8 * GIB);
    let cfg = write_root_config(&h, &assets, &image);
    let mut vm = h.spawn_run(&cfg);
    wait_root(&mut vm);
    type_line(
        &mut vm,
        "command -v resize2fs >/dev/null && resize2fs /dev/vda; echo FS_READY\n",
    );
    vm.wait_serial("fs ready", Duration::from_secs(30), |s| {
        s.contains("FS_READY")
    });
    type_line(
        &mut vm,
        "dd if=/dev/zero of=/big.bin bs=1M count=2048 conv=fsync && echo BIG_DONE || echo BIG_FAIL\n",
    );
    vm.wait_serial("2g write", Duration::from_secs(300), |s| {
        s.contains("BIG_DONE") || s.contains("BIG_FAIL")
    });
    assert!(
        vm.serial().contains("BIG_DONE"),
        "guest 2 GiB file failed: {}",
        vm.serial()
    );
    let before = du_k(&image);
    let logical_before = logical(&image);
    type_line(
        &mut vm,
        "rm -f /big.bin; sync; fstrim -v /; sync; echo TRIM_DONE\n",
    );
    vm.wait_serial("fstrim", Duration::from_secs(120), |s| {
        s.contains("TRIM_DONE")
    });
    std::thread::sleep(Duration::from_secs(2));
    let after = du_k(&image);
    let logical_after = logical(&image);
    tracing::info!(target: T, before, after, logical_before, logical_after, "fstrim du");
    assert_eq!(logical_before, logical_after);
    assert_eq!(logical_after, 8 * GIB);
    assert!(
        after < before,
        "du -k should drop after fstrim ({before} -> {after})"
    );
    type_line(&mut vm, "poweroff -f\n");
    let _ = vm.wait_exit(Duration::from_secs(30));
}

#[test]
#[ignore = "needs-hv"]
fn hv_online_grow_growpart_resize2fs() {
    let Some(assets) = root_assets() else {
        return;
    };
    let h = Harness::new("dskgrow");
    let image = sparse_root(&h, &assets, 8 * GIB);
    let cfg = write_root_config(&h, &assets, &image);
    let mut vm = h.spawn_run(&cfg);
    wait_root(&mut vm);
    wait_socket(&h);
    type_line(&mut vm, "cat /sys/block/vda/size; echo SIZE8\n");
    vm.wait_serial("8g sectors", Duration::from_secs(20), |s| {
        s.contains(SECTORS_8G) && s.contains("SIZE8")
    });
    assert_ok(&h.tv(&["disk", "resize", &h.name, "16G"]), "resize");
    type_line(&mut vm, "cat /sys/block/vda/size; echo SIZE16\n");
    vm.wait_serial("16g sectors", Duration::from_secs(20), |s| {
        s.contains(SECTORS_16G) && s.contains("SIZE16")
    });
    type_line(
        &mut vm,
        "command -v growpart >/dev/null && growpart /dev/vda 1; command -v resize2fs >/dev/null && resize2fs /dev/vda; command -v resize2fs >/dev/null && resize2fs /dev/vda1; df -h /; echo GROW_DONE\n",
    );
    vm.wait_serial("grow tools", Duration::from_secs(30), |s| {
        s.contains("GROW_DONE")
    });
    assert_eq!(logical(&image), 16 * GIB);
    type_line(&mut vm, "poweroff -f\n");
    let _ = vm.wait_exit(Duration::from_secs(30));
}
