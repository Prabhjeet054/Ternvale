use std::path::{Path, PathBuf};

use ternvale_config::{Disk, VmConfig};

use super::{
    check_cmdline, check_disk, check_initrd, check_kernel, checks_for_path, initrd_format,
    root_disk, RootDisk, EXT_MAGIC_AT,
};
use crate::doctor::Outcome;

fn scratch(test: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "ternvale-cli-vmcheck-{}-{test}",
        std::process::id()
    ));
    if dir.exists() {
        std::fs::remove_dir_all(&dir).expect("clear leftovers");
    }
    std::fs::create_dir_all(&dir).expect("dir");
    dir
}

fn image(dir: &Path) -> PathBuf {
    let mut bytes = vec![0u8; 4096];
    bytes[56..60].copy_from_slice(b"ARM\x64");
    let path = dir.join("Image");
    std::fs::write(&path, bytes).expect("image");
    path
}

fn ext_disk(dir: &Path, magic: u16) -> PathBuf {
    let mut bytes = vec![0u8; 8192];
    let at = EXT_MAGIC_AT as usize;
    bytes[at..at + 2].copy_from_slice(&magic.to_le_bytes());
    let path = dir.join(format!("disk-{magic:04x}.ext4"));
    std::fs::write(&path, bytes).expect("disk");
    path
}

fn config(disks: usize, initrd: bool) -> VmConfig {
    let mut text = String::from(
        "name = \"t\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n",
    );
    if initrd {
        text.push_str("initrd = \"/i\"\n");
    }
    for _ in 0..disks {
        text.push_str("\n[[disks]]\npath = \"/d\"\nread_only = false\n");
    }
    VmConfig::parse(&text).expect("parse")
}

#[test]
fn root_disk_reads_the_last_root_and_rootfstype() {
    assert_eq!(
        root_disk("console=ttyAMA0 root=/dev/vda rootfstype=ext4 rw"),
        Some(RootDisk {
            index: 0,
            device: 'a',
            partition: false,
            ext: true
        })
    );
    let part = root_disk("root=/dev/vdb2").expect("vdb2");
    assert_eq!((part.index, part.partition, part.ext), (1, true, true));
    assert!(
        !root_disk("root=/dev/vda rootfstype=btrfs")
            .expect("vda")
            .ext
    );
    assert_eq!(
        root_disk("root=/dev/vda root=/dev/vdc").map(|r| r.index),
        Some(2)
    );
    assert_eq!(root_disk("root=PARTUUID=1234"), None);
    assert_eq!(root_disk("root=/dev/vdA"), None);
    assert_eq!(root_disk("rdinit=/init"), None);
}

#[test]
fn initrd_formats_are_recognised() {
    assert_eq!(initrd_format(b"070701000"), Some("newc cpio"));
    assert_eq!(initrd_format(&[0x1f, 0x8b, 8]), Some("gzip"));
    assert_eq!(initrd_format(&[0x28, 0xb5, 0x2f, 0xfd]), Some("zstd"));
    assert_eq!(initrd_format(b"hello!"), None);

    let dir = scratch("initrd");
    let empty = dir.join("empty.cpio");
    std::fs::write(&empty, b"").expect("empty");
    assert_eq!(check_initrd(&empty).outcome, Outcome::Fail);
    let odd = dir.join("odd.cpio");
    std::fs::write(&odd, b"hello world").expect("odd");
    assert_eq!(check_initrd(&odd).outcome, Outcome::Warn);
}

#[test]
fn kernel_check_names_the_problem() {
    let dir = scratch("kernel");
    let good = check_kernel(&image(&dir));
    assert_eq!(good.outcome, Outcome::Pass, "{good:?}");
    assert!(
        good.detail.ends_with(": arm64 Image, 4.0 KiB"),
        "{}",
        good.detail
    );

    let missing = check_kernel(&dir.join("missing/Image"));
    assert_eq!(missing.outcome, Outcome::Fail);
    assert!(
        missing
            .detail
            .contains("missing/Image: No such file or directory"),
        "{}",
        missing.detail
    );
    assert!(missing
        .fix
        .as_deref()
        .is_some_and(|f| f.contains("fetch-test-kernel.sh")));

    let mut zboot = vec![0u8; 128];
    zboot[..2].copy_from_slice(b"MZ");
    zboot[4..8].copy_from_slice(b"zimg");
    std::fs::write(dir.join("vmlinuz"), &zboot).expect("zboot");
    let zboot = check_kernel(&dir.join("vmlinuz"));
    assert!(
        zboot
            .detail
            .ends_with("is an EFI zboot vmlinuz (compressed), not a raw Image"),
        "{}",
        zboot.detail
    );

    std::fs::write(dir.join("x86"), vec![0x90u8; 128]).expect("x86");
    let x86 = check_kernel(&dir.join("x86"));
    assert!(
        x86.detail
            .contains("offset 56 holds 0x90909090, want 0x644d5241"),
        "{}",
        x86.detail
    );
}

#[test]
fn disk_check_finds_a_zeroed_superblock_on_the_root_disk() {
    let dir = scratch("disk");
    let disk = |path: PathBuf| Disk {
        path,
        read_only: false,
    };
    let good = check_disk(0, &disk(ext_disk(&dir, 0xef53)), Some('a'));
    assert_eq!(good.outcome, Outcome::Pass, "{good:?}");
    assert!(
        good.detail
            .ends_with(": 8.0 KiB, read-write, ext root /dev/vda"),
        "{}",
        good.detail
    );

    let bad = check_disk(0, &disk(ext_disk(&dir, 0)), Some('a'));
    assert_eq!(bad.name, "disk 0");
    assert_eq!(bad.outcome, Outcome::Fail);
    assert!(
        bad.detail.ends_with("has no ext2/3/4 superblock (bytes 1080..1082 hold 0x0000, want 0xef53), so the guest cannot mount root=/dev/vda"),
        "{}",
        bad.detail
    );
    let data = check_disk(1, &disk(ext_disk(&dir, 0)), None);
    assert_eq!(
        data.outcome,
        Outcome::Pass,
        "a data disk is not checked for ext"
    );

    let missing = check_disk(2, &disk(dir.join("gone.img")), None);
    assert!(
        missing.detail.contains("cannot open read-write"),
        "{}",
        missing.detail
    );
    std::fs::write(dir.join("odd.img"), vec![0u8; 1000]).expect("odd");
    assert_eq!(
        check_disk(0, &disk(dir.join("odd.img")), None).outcome,
        Outcome::Warn
    );
}

#[test]
fn cmdline_check_catches_wrong_console_and_root() {
    let one = config(1, false);
    let wrong = check_cmdline("console=ttyS0 root=/dev/vda rootfstype=ext4 rw", &one);
    assert_eq!(wrong.len(), 1);
    assert_eq!(wrong[0].outcome, Outcome::Fail);
    assert!(
        wrong[0]
            .detail
            .starts_with("console=ttyS0 is not a device on this VM"),
        "{}",
        wrong[0].detail
    );

    let last = check_cmdline("console=ttyAMA0 console=tty0 root=/dev/vda", &one);
    assert_eq!(last[0].outcome, Outcome::Warn);
    assert!(
        last[0].detail.contains("console=tty0 comes last"),
        "{}",
        last[0].detail
    );

    let root = check_cmdline("console=ttyAMA0 root=/dev/vdb", &one);
    assert_eq!(root[0].detail, "root=/dev/vdb but the VM has 1 disk(s)");

    let nothing = check_cmdline("console=ttyAMA0", &config(0, false));
    assert!(
        nothing[0].detail.starts_with("no root= and no initrd"),
        "{}",
        nothing[0].detail
    );

    let fine = check_cmdline(ternvale_vmm::DISK_ROOT_CMDLINE, &one);
    assert_eq!(fine[0].outcome, Outcome::Pass);
    let stdout_path = check_cmdline("ternvale.agent=info root=/dev/vda", &one);
    assert_eq!(
        stdout_path[0].outcome,
        Outcome::Pass,
        "no console= follows stdout-path"
    );
}

#[test]
fn config_path_checks_report_every_broken_field() {
    let dir = scratch("config");
    let bad = ext_disk(&dir, 0);
    let path = dir.join("vm.toml");
    std::fs::write(
        &path,
        format!(
            "name = \"t\"\ncpus = 1\nram_mib = 256\nkernel = \"{}\"\nserial_log = \"{}\"\nboot_disk = true\ncmdline = \"console=ttyS0\"\n\n[[disks]]\npath = \"{}\"\nread_only = false\n",
            dir.join("missing/Image").display(),
            dir.join("serial.log").display(),
            bad.display()
        ),
    )
    .expect("config");
    let checks = checks_for_path(&path);
    let names: Vec<(&str, Outcome)> = checks
        .iter()
        .map(|c| (c.name.as_str(), c.outcome))
        .collect();
    assert_eq!(
        names,
        [
            ("vm config", Outcome::Fail),
            ("kernel", Outcome::Fail),
            ("disk 0", Outcome::Fail),
            ("cmdline", Outcome::Fail),
        ]
    );
    assert!(checks[0].detail.contains("kernel"), "{}", checks[0].detail);

    std::fs::write(&path, "name = ").expect("broken");
    let broken = checks_for_path(&path);
    assert_eq!(broken.len(), 1);
    assert_eq!(broken[0].outcome, Outcome::Fail);
}
