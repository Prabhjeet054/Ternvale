//! `ternvale doctor --config`: problems in one VM config that stop the guest
//! booting or hide its output.
//!
//! The checks read only the config and the first bytes of the files it names.
//! `ternvale run` runs [`checks`] after loading the config and logs every
//! problem as a WARN, so `ternvale logs <name> --level warn` shows it too.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use ternvale_config::{Disk, VmConfig};
use ternvale_vmm::{resolve_cmdline, IMAGE_MAGIC};

use super::{human, Check, Outcome};

/// The PL011 is the guest's only serial console.
pub const SERIAL_CONSOLE: &str = "ttyAMA0";
/// Byte offset of `ARM\x64` in an arm64 Image header.
const IMAGE_MAGIC_AT: usize = 56;
/// Byte offset of `s_magic` in an ext2/3/4 superblock (1024 + 0x38).
pub const EXT_MAGIC_AT: u64 = 1080;
pub const EXT_MAGIC: u16 = 0xef53;
const SECTOR: u64 = 512;

/// Parse `path` without stopping at the first error, then run [`checks`].
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(config = %path.display()))]
pub fn checks_for_path(path: &Path) -> Vec<Check> {
    const NAME: &str = "vm config";
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) => {
            return vec![Check::new(
                NAME,
                Outcome::Fail,
                format!("cannot read {}: {error}", path.display()),
            )
            .fix("Pass the path of a VM config TOML.")]
        }
    };
    let config = match VmConfig::parse(&text) {
        Ok(config) => config,
        Err(error) => {
            return vec![
                Check::new(NAME, Outcome::Fail, format!("{}: {error}", path.display()))
                    .fix("Fix the TOML; see the config section of docs/ARCHITECTURE.md."),
            ]
        }
    };
    let verdict = match config.validate() {
        Ok(()) => Check::new(
            NAME,
            Outcome::Pass,
            format!(
                "{} is valid (vm {}, {} cpus, {} MiB)",
                path.display(),
                config.name,
                config.cpus,
                config.ram_mib
            ),
        ),
        Err(error) => Check::new(NAME, Outcome::Fail, format!("{}: {error}", path.display()))
            .fix("`ternvale run` refuses this config; the checks below name the field at fault."),
    };
    let mut out = vec![verdict];
    out.extend(checks(&config));
    out
}

/// Boot image, initrd, disks, and cmdline of a parsed config.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(vm = %config.name))]
pub fn checks(config: &VmConfig) -> Vec<Check> {
    let mut out = Vec::new();
    let cmdline = match &config.firmware {
        Some(firmware) => {
            out.push(check_firmware(firmware));
            None
        }
        None => {
            out.push(check_kernel(&config.kernel));
            Some(resolve_cmdline(&config.cmdline, config.boot_disk))
        }
    };
    if let Some(initrd) = &config.initrd {
        out.push(check_initrd(initrd));
    }
    let root = cmdline.as_deref().and_then(root_disk);
    for (index, disk) in config.disks.iter().enumerate() {
        let ext_root = root.filter(|r| r.index == index && r.ext && !r.partition);
        out.push(check_disk(index, disk, ext_root.map(|r| r.device)));
    }
    if let Some(cmdline) = &cmdline {
        out.extend(check_cmdline(cmdline, config));
    }
    for check in out.iter().filter(|c| c.outcome != Outcome::Pass) {
        tracing::debug!(target: "ternvale::cli", check = %check.name, outcome = ?check.outcome, detail = %check.detail, "vm config problem");
    }
    out
}

/// The disk `root=` names, when it is a whole virtio disk or one of its partitions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RootDisk {
    pub index: usize,
    pub device: char,
    /// `root=/dev/vda1` and the like: the filesystem is not at byte 0.
    pub partition: bool,
    /// `rootfstype` is absent or ext2/3/4, which share the superblock magic.
    pub ext: bool,
}

/// `root=/dev/vdX[N]` in `cmdline`. Disk `i` is `/dev/vd<'a'+i>` because config
/// disks take virtio-mmio slots in order and Linux probes them in DT order.
#[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
pub fn root_disk(cmdline: &str) -> Option<RootDisk> {
    let value = arg(cmdline, "root")?;
    let rest = value.strip_prefix("/dev/vd")?;
    let device = rest.chars().next().filter(char::is_ascii_lowercase)?;
    let tail = &rest[1..];
    if !tail.chars().all(|c| c.is_ascii_digit()) {
        return None;
    }
    let ext = arg(cmdline, "rootfstype").is_none_or(|t| matches!(t, "ext2" | "ext3" | "ext4"));
    Some(RootDisk {
        index: usize::from(device as u8 - b'a'),
        device,
        partition: !tail.is_empty(),
        ext,
    })
}

/// The value of the last `key=` token (the kernel keeps the last one).
fn arg<'a>(cmdline: &'a str, key: &str) -> Option<&'a str> {
    cmdline
        .split_whitespace()
        .filter_map(|token| token.strip_prefix(key)?.strip_prefix('='))
        .next_back()
}

fn head(path: &Path, len: usize) -> std::io::Result<(Vec<u8>, u64)> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    let mut bytes = Vec::with_capacity(len);
    file.by_ref().take(len as u64).read_to_end(&mut bytes)?;
    Ok((bytes, size))
}

/// The kernel is an uncompressed arm64 Linux Image.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(kernel = %path.display()))]
pub fn check_kernel(path: &Path) -> Check {
    const NAME: &str = "kernel";
    const FIX: &str = "Point `kernel` at an uncompressed arm64 Linux Image, e.g. test-assets/Image from scripts/fetch-test-kernel.sh (docs/BOOT.md).";
    if path.as_os_str().is_empty() {
        return Check::new(
            NAME,
            Outcome::Fail,
            "no kernel and no firmware in the config",
        )
        .fix(FIX);
    }
    let (bytes, size) = match head(path, 64) {
        Ok(read) => read,
        Err(error) => {
            return Check::new(NAME, Outcome::Fail, format!("{}: {error}", path.display())).fix(FIX)
        }
    };
    let shown = path.display();
    if bytes.len() < 64 {
        return Check::new(
            NAME,
            Outcome::Fail,
            format!("{shown} is {size} bytes, too short for an arm64 Image header"),
        )
        .fix(FIX);
    }
    let magic = u32::from_le_bytes([bytes[56], bytes[57], bytes[58], bytes[59]]);
    if magic == IMAGE_MAGIC {
        return Check::new(
            NAME,
            Outcome::Pass,
            format!("{shown}: arm64 Image, {}", human(size)),
        );
    }
    if bytes.starts_with(b"MZ") && &bytes[4..8] == b"zimg" {
        return Check::new(NAME, Outcome::Fail, format!("{shown} is an EFI zboot vmlinuz (compressed), not a raw Image"))
            .fix("Unpack it the way scripts/fetch-test-kernel.sh does, or boot it through UEFI `firmware`.");
    }
    if bytes.starts_with(&[0x1f, 0x8b]) {
        return Check::new(NAME, Outcome::Fail, format!("{shown} is gzip-compressed")).fix(
            format!("`gunzip -c {shown} > Image` and point `kernel` at the result."),
        );
    }
    Check::new(
        NAME,
        Outcome::Fail,
        format!("{shown} is not an arm64 Image: offset {IMAGE_MAGIC_AT} holds {magic:#010x}, want {IMAGE_MAGIC:#010x} (\"ARM\\x64\")"),
    )
    .fix(FIX)
}

/// The firmware image exists and is not empty.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(firmware = %path.display()))]
pub fn check_firmware(path: &Path) -> Check {
    const NAME: &str = "firmware";
    const FIX: &str = "Point `firmware` at QEMU_EFI.fd from scripts/fetch-firmware.sh.";
    match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() > 0 => Check::new(
            NAME,
            Outcome::Pass,
            format!("{}: {}", path.display(), human(meta.len())),
        ),
        Ok(_) => Check::new(
            NAME,
            Outcome::Fail,
            format!("{} is empty or not a file", path.display()),
        )
        .fix(FIX),
        Err(error) => {
            Check::new(NAME, Outcome::Fail, format!("{}: {error}", path.display())).fix(FIX)
        }
    }
}

/// What the first bytes of an initrd say it is, if the kernel can unpack it.
#[tracing::instrument(level = "trace", target = "ternvale::cli", skip_all)]
pub fn initrd_format(bytes: &[u8]) -> Option<&'static str> {
    const FORMATS: [(&[u8], &str); 7] = [
        (b"070701", "newc cpio"),
        (b"070702", "newc cpio (crc)"),
        (&[0x1f, 0x8b], "gzip"),
        (&[0xfd, b'7', b'z', b'X', b'Z', 0], "xz"),
        (&[0x28, 0xb5, 0x2f, 0xfd], "zstd"),
        (&[0x02, 0x21, 0x4c, 0x18], "lz4"),
        (b"BZh", "bzip2"),
    ];
    FORMATS
        .iter()
        .find(|(magic, _)| bytes.starts_with(magic))
        .map(|(_, name)| *name)
}

/// The initrd exists and looks like an archive the kernel can unpack.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(initrd = %path.display()))]
pub fn check_initrd(path: &Path) -> Check {
    const NAME: &str = "initrd";
    const FIX: &str =
        "Point `initrd` at a newc cpio (`cpio -o -H newc`, optionally gzipped), or remove it.";
    let shown = path.display();
    match head(path, 6) {
        Err(error) => Check::new(NAME, Outcome::Fail, format!("{shown}: {error}")).fix(FIX),
        Ok((_, 0)) => Check::new(NAME, Outcome::Fail, format!("{shown} is empty")).fix(FIX),
        Ok((bytes, size)) => match initrd_format(&bytes) {
            Some(format) => Check::new(NAME, Outcome::Pass, format!("{shown}: {format}, {}", human(size))),
            None => Check::new(
                NAME,
                Outcome::Warn,
                format!("{shown} is not a newc cpio or a compressed archive the kernel knows (starts with {bytes:02x?})"),
            )
            .fix(FIX),
        },
    }
}

/// The disk opens in its mode, is sector-sized, and, when it is the ext root,
/// has an ext superblock.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(index, disk = %disk.path.display()))]
pub fn check_disk(index: usize, disk: &Disk, ext_root: Option<char>) -> Check {
    let name = format!("disk {index}");
    let shown = disk.path.display();
    let mode = if disk.read_only {
        "read-only"
    } else {
        "read-write"
    };
    let opened = OpenOptions::new()
        .read(true)
        .write(!disk.read_only)
        .open(&disk.path);
    let mut file = match opened {
        Ok(file) => file,
        Err(error) => {
            return Check::new(name, Outcome::Fail, format!("{shown}: cannot open {mode}: {error}")).fix(format!(
                "Fix `disks[{index}].path` (`ternvale create-disk <path> <size>` makes a blank image), check its permissions, or set read_only = true."
            ))
        }
    };
    let size = match file.metadata() {
        Ok(meta) => meta.len(),
        Err(error) => return Check::new(name, Outcome::Fail, format!("{shown}: {error}")),
    };
    if size == 0 {
        return Check::new(name, Outcome::Fail, format!("{shown} is empty (0 bytes)"))
            .fix("Use a real image; `ternvale create-disk <path> <size>` makes a blank one.");
    }
    if let Some(device) = ext_root {
        let mut magic = [0u8; 2];
        let read = file
            .seek(SeekFrom::Start(EXT_MAGIC_AT))
            .and_then(|_| file.read_exact(&mut magic));
        let found = u16::from_le_bytes(magic);
        if read.is_err() || found != EXT_MAGIC {
            return Check::new(
                name,
                Outcome::Fail,
                format!(
                    "{shown} has no ext2/3/4 superblock (bytes {EXT_MAGIC_AT}..{} hold {found:#06x}, want {EXT_MAGIC:#06x}), so the guest cannot mount root=/dev/vd{device}",
                    EXT_MAGIC_AT + 2
                ),
            )
            .fix("Restore the image from a good copy (scripts/make-rootfs.sh rebuilds test-assets/virtio-root/rootfs.ext4); scripts/fsck-rootfs.sh <image> shows what e2fsck sees.");
        }
    }
    let root = ext_root.map_or(String::new(), |d| format!(", ext root /dev/vd{d}"));
    let detail = format!("{shown}: {}, {mode}{root}", human(size));
    if size % SECTOR != 0 {
        return Check::new(
            name,
            Outcome::Warn,
            format!("{detail}; not a multiple of {SECTOR} bytes"),
        )
        .fix(format!(
            "The guest sees only {} whole sectors; resize the image to a multiple of {SECTOR}.",
            size / SECTOR
        ));
    }
    Check::new(name, Outcome::Pass, detail)
}

/// `console=` reaches the PL011 and `root=` names a disk the VM has.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(cmdline))]
pub fn check_cmdline(cmdline: &str, config: &VmConfig) -> Vec<Check> {
    const NAME: &str = "cmdline";
    let mut out = Vec::new();
    let consoles: Vec<&str> = cmdline
        .split_whitespace()
        .filter_map(|t| t.strip_prefix("console="))
        .map(|c| c.split(',').next().unwrap_or(c))
        .collect();
    if !consoles.is_empty() && !consoles.contains(&SERIAL_CONSOLE) {
        out.push(
            Check::new(
                NAME,
                Outcome::Fail,
                format!(
                    "console={} is not a device on this VM, so kernel and init output never reach serial_log (the only console is the PL011, {SERIAL_CONSOLE})",
                    consoles.join(",console=")
                ),
            )
            .fix(format!("Use console={SERIAL_CONSOLE} (plus earlycon=pl011,0x9000000 for early messages), or drop console= to follow the device tree's stdout-path.")),
        );
    } else if let Some(last) = consoles.last().filter(|c| **c != SERIAL_CONSOLE) {
        out.push(
            Check::new(
                NAME,
                Outcome::Warn,
                format!("console={last} comes last, so /dev/console (init and the shell) is {last}, not the serial log"),
            )
            .fix(format!("Put console={SERIAL_CONSOLE} last.")),
        );
    }
    if let Some(root) = root_disk(cmdline).filter(|r| r.index >= config.disks.len()) {
        out.push(
            Check::new(
                NAME,
                Outcome::Fail,
                format!(
                    "root=/dev/vd{} but the VM has {} disk(s)",
                    root.device,
                    config.disks.len()
                ),
            )
            .fix("Add the disk under [[disks]], or point root= at /dev/vda (the first disk)."),
        );
    }
    if arg(cmdline, "root").is_none() && config.initrd.is_none() {
        out.push(
            Check::new(
                NAME,
                Outcome::Warn,
                "no root= and no initrd: the kernel has nothing to mount",
            )
            .fix("Set boot_disk = true with a [[disks]] entry, or add an initrd."),
        );
    }
    if out.is_empty() {
        out.push(Check::new(NAME, Outcome::Pass, format!("`{cmdline}`")));
    }
    out
}

#[cfg(test)]
#[path = "vm_tests.rs"]
mod tests;
