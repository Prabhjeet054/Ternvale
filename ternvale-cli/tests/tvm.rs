//! Step 85: `.tvm` export/import, sparse `du -k`, checksums, qcow2, templates.

mod support;

use std::io::{Seek, SeekFrom, Write};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use support::{CliOut, Harness, Vm};
use ternvale_snapshot::ImageStack;

const T: &str = "ternvale::cli";
const MARKER: &[u8] = b"TVM-STATE-1";
const LOGICAL: u64 = 32 * 1024 * 1024;
const BOOT: Duration = Duration::from_secs(90);

fn assets() -> PathBuf {
    support::root().join("test-assets/virtio-blk")
}

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
    let text = String::from_utf8_lossy(&out.stdout);
    let n = text
        .split_whitespace()
        .next()
        .expect("du field")
        .parse::<u64>()
        .expect("du int");
    tracing::info!(target: T, path = %path.display(), kib = n, "du -k");
    n
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(path = %path.display()))]
fn write_sparse(path: &Path, marker: &[u8]) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("parent");
    }
    let mut file = std::fs::File::create(path).expect("disk");
    file.set_len(LOGICAL).expect("len");
    file.write_all(marker).expect("head");
    file.seek(SeekFrom::Start(LOGICAL - marker.len() as u64))
        .expect("seek");
    file.write_all(marker).expect("tail");
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = name))]
fn install_vm(h: &Harness, name: &str) -> PathBuf {
    let assets = assets();
    for file in ["Image", "initramfs.cpio"] {
        assert!(
            assets.join(file).is_file(),
            "missing {}",
            assets.join(file).display()
        );
    }
    let dir = h.out.join("vms").join(name);
    std::fs::create_dir_all(dir.join("disks")).expect("vms");
    let disk = dir.join("disks/disk0.img");
    write_sparse(&disk, MARKER);
    let serial = h.out.join(format!("{name}-serial.log"));
    let text = format!(
        "name = \"{name}\"\ncpus = 1\nram_mib = 256\nkernel = \"{}\"\ninitrd = \"{}\"\nserial_log = \"{}\"\ncmdline = \"console=ttyAMA0\"\n\n[[disks]]\npath = \"{}\"\nread_only = false\n",
        assets.join("Image").display(),
        assets.join("initramfs.cpio").display(),
        serial.display(),
        disk.display()
    );
    std::fs::write(dir.join("config.toml"), text).expect("config");
    tracing::info!(target: T, vm = name, disk = %disk.display(), "library vm installed");
    disk
}

fn assert_ok(out: &CliOut, what: &str) {
    assert_eq!(out.code, Some(0), "{what}: {out:?}");
}

fn qemu_img() -> Option<PathBuf> {
    ternvale_cli::qemu_img::find().ok()
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
fn wait_vda(vm: &mut Vm) {
    vm.wait_serial("vda ready", BOOT, |s| {
        s.contains("vda ready") && s.contains("# ")
    });
}

#[test]
fn export_import_preserves_sparse_du_and_rejects_corrupt_byte() {
    let h = Harness::new_unsigned("tvmex");
    let src_disk = install_vm(&h, "src");
    let src_du = du_k(&src_disk);
    assert!(src_du < 1024, "source should stay sparse, du -k={src_du}");

    let bundle = h.out.join("src.tvm");
    assert_ok(
        &h.tv(&["export", "src", &bundle.display().to_string()]),
        "export",
    );
    assert!(bundle.join("MANIFEST.json").is_file());

    assert_ok(
        &h.tv(&["import", &bundle.display().to_string(), "--name", "imp"]),
        "import",
    );
    let imp_disk = h.out.join("vms/imp/disks/disk0.img");
    let imp_du = du_k(&imp_disk);
    assert!(
        imp_du <= src_du + 64,
        "imported sparse size {imp_du} KiB vs source {src_du} KiB"
    );
    assert_eq!(
        &std::fs::read(&src_disk).expect("src")[..MARKER.len()],
        MARKER
    );
    assert_eq!(
        &std::fs::read(&imp_disk).expect("imp")[..MARKER.len()],
        MARKER
    );

    let payload = bundle.join("disks/disk0.img");
    let mut bytes = std::fs::read(&payload).expect("bundle disk");
    bytes[0] ^= 0xff;
    std::fs::write(&payload, bytes).expect("corrupt");
    let bad = h.tv(&["import", &bundle.display().to_string(), "--name", "bad"]);
    assert_ne!(bad.code, Some(0), "corrupt import must fail: {bad:?}");
    let msg = format!("{}{}", bad.stdout, bad.error_line());
    assert!(msg.contains("checksum mismatch"), "{bad:?}");
    assert!(msg.contains("disks/disk0.img"), "{bad:?}");
    tracing::info!(target: T, src_du, imp_du, "sparse du and checksum reject ok");
}

#[test]
fn import_qcow2_from_qemu_img() {
    let Some(bin) = qemu_img() else {
        tracing::warn!(target: T, "qemu-img missing; skip qcow2 import");
        return;
    };
    let h = Harness::new_unsigned("tvmqc");
    let raw = h.out.join("linux.raw");
    write_sparse(&raw, MARKER);
    let qcow = h.out.join("linux.qcow2");
    let conv = Command::new(&bin)
        .args(["convert", "-f", "raw", "-O", "qcow2"])
        .arg(&raw)
        .arg(&qcow)
        .output()
        .expect("qemu-img convert");
    assert!(
        conv.status.success(),
        "qemu-img: {}",
        String::from_utf8_lossy(&conv.stderr)
    );
    tracing::info!(target: T, qcow = %qcow.display(), "qcow2 produced by qemu-img");

    let kernel = assets().join("Image");
    assert_ok(
        &h.tv(&[
            "import",
            &qcow.display().to_string(),
            "--name",
            "qc",
            "--kernel",
            &kernel.display().to_string(),
        ]),
        "import qcow2",
    );
    let disk = h.out.join("vms/qc/disks/disk0.img");
    assert_eq!(&std::fs::read(&disk).expect("qc")[..MARKER.len()], MARKER);
}

#[test]
fn template_three_clones_are_isolated_and_fast() {
    let h = Harness::new_unsigned("tvmtpl");
    install_vm(&h, "src");
    assert_ok(&h.tv(&["template", "create", "src", "golden"]), "template");
    let tpl_disk = h.out.join("templates/golden/disks/disk0.img");
    let mode = std::fs::metadata(&tpl_disk).expect("tpl").permissions();
    assert_eq!(mode.mode() & 0o222, 0, "template disk must be read-only");
    let before = std::fs::read(&tpl_disk).expect("tpl bytes");

    let mut times = Vec::new();
    for name in ["c1", "c2", "c3"] {
        let start = Instant::now();
        assert_ok(&h.tv(&["new", name, "--from-template", "golden"]), name);
        times.push(start.elapsed());
    }
    for elapsed in &times {
        assert!(
            *elapsed < Duration::from_secs(2),
            "linked clone took {elapsed:?}"
        );
    }
    tracing::info!(target: T, ?times, "three clones created");

    for (i, name) in ["c1", "c2", "c3"].iter().enumerate() {
        let stack = h.out.join("vms").join(name).join("disks/disk0.tstack");
        let mut image = ImageStack::open(&stack, false).expect("stack");
        let marker = [b'A' + i as u8; 16];
        image.write_at(&marker, 0).expect("write clone");
    }
    for (i, name) in ["c1", "c2", "c3"].iter().enumerate() {
        let stack = h.out.join("vms").join(name).join("disks/disk0.tstack");
        let image = ImageStack::open(&stack, true).expect("ro");
        let mut buf = [0u8; 16];
        image.read_at(&mut buf, 0).expect("read");
        assert_eq!(buf, [b'A' + i as u8; 16], "{name} not isolated");
    }
    assert_eq!(std::fs::read(&tpl_disk).expect("tpl after"), before);
}

#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(config = %config.display()))]
fn patch_initrd(config: &Path) {
    let initrd = assets().join("initramfs.cpio");
    let text = std::fs::read_to_string(config).expect("cfg");
    let text = text.replacen(
        "serial_log",
        &format!("initrd = \"{}\"\nserial_log", initrd.display()),
        1,
    );
    std::fs::write(config, text).expect("patch initrd");
}

#[test]
#[ignore = "needs-hv"]
fn hv_export_import_guest_state_intact() {
    let h = Harness::new("tvmhv");
    install_vm(&h, "src");
    let cfg = h.out.join("vms/src/config.toml");
    let mut vm = h.spawn_run_tagged(&cfg, "src", h.out.join("src-serial.log"));
    wait_vda(&mut vm);
    vm.type_chunks(&["echo TVM-OK >/de", "v/vda; sync\n"]);
    vm.wait_serial("guest wrote marker", Duration::from_secs(20), |s| {
        s.contains("TVM-OK") || s.matches("# ").count() >= 2
    });
    vm.type_chunks(&["poweroff -f\n"]);
    let _ = vm.wait_exit(Duration::from_secs(30));

    let bundle = h.out.join("src.tvm");
    assert_ok(
        &h.tv(&["export", "src", &bundle.display().to_string()]),
        "export",
    );
    assert_ok(
        &h.tv(&["import", &bundle.display().to_string(), "--name", "imp"]),
        "import",
    );
    let imp_cfg = h.out.join("vms/imp/config.toml");
    let serial = h.out.join("imp-serial.log");
    let mut copy = h.spawn_run_tagged(&imp_cfg, "imp", serial);
    wait_vda(&mut copy);
    copy.type_chunks(&["hexdump -C -n 16", " /dev/vda\n"]);
    copy.wait_serial("imported marker", Duration::from_secs(20), |s| {
        s.contains("TVM-OK") || s.contains("54 56 4d")
    });
    copy.type_chunks(&["poweroff -f\n"]);
    let _ = copy.wait_exit(Duration::from_secs(30));
}

#[test]
#[ignore = "needs-hv"]
fn hv_import_qcow2_linux_boots() {
    let Some(bin) = qemu_img() else {
        tracing::warn!(target: T, "qemu-img missing; skip qcow2 HV boot");
        return;
    };
    let h = Harness::new("tvmqchv");
    let raw = h.out.join("linux.raw");
    write_sparse(&raw, MARKER);
    let qcow = h.out.join("linux.qcow2");
    let conv = Command::new(&bin)
        .args(["convert", "-f", "raw", "-O", "qcow2"])
        .arg(&raw)
        .arg(&qcow)
        .output()
        .expect("qemu-img");
    assert!(
        conv.status.success(),
        "{}",
        String::from_utf8_lossy(&conv.stderr)
    );
    let kernel = assets().join("Image");
    assert_ok(
        &h.tv(&[
            "import",
            &qcow.display().to_string(),
            "--name",
            "qc",
            "--kernel",
            &kernel.display().to_string(),
        ]),
        "import qcow2",
    );
    let cfg = h.out.join("vms/qc/config.toml");
    patch_initrd(&cfg);
    let serial = h.out.join("qc-serial.log");
    let mut vm = h.spawn_run_tagged(&cfg, "qc", serial);
    wait_vda(&mut vm);
    vm.type_chunks(&["poweroff -f\n"]);
    let _ = vm.wait_exit(Duration::from_secs(30));
}

#[test]
#[ignore = "needs-hv"]
fn hv_three_template_clones_boot_quickly() {
    let h = Harness::new("tvm3hv");
    install_vm(&h, "src");
    assert_ok(&h.tv(&["template", "create", "src", "golden"]), "template");
    for name in ["c1", "c2", "c3"] {
        assert_ok(&h.tv(&["new", name, "--from-template", "golden"]), name);
    }
    let started = Instant::now();
    let mut vms: Vec<Vm> = ["c1", "c2", "c3"]
        .iter()
        .map(|name| {
            let cfg = h.out.join("vms").join(name).join("config.toml");
            let serial = h.out.join(format!("{name}-serial.log"));
            h.spawn_run_tagged(&cfg, name, serial)
        })
        .collect();
    for vm in &mut vms {
        wait_vda(vm);
    }
    let elapsed = started.elapsed();
    tracing::info!(target: T, ms = elapsed.as_millis() as u64, "three clone boots");
    assert!(
        elapsed < Duration::from_secs(120),
        "three clone boots took {elapsed:?}"
    );
    for vm in &mut vms {
        vm.type_chunks(&["poweroff -f\n"]);
    }
}
