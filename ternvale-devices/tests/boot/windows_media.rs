//! Windows media scenario: the `[iso]` installer and driver slots as
//! read-only optical virtio-pci devices, attached by `attach_blocks` exactly
//! as `ternvale run` does.
//!
//! Assets: the Alpine ISO stands in for the Windows installer
//! (`./scripts/fetch-installer-iso.sh`); the driver disc is the real virtio-win
//! ISO (`./scripts/fetch-virtio-win.sh`). Two boots:
//! - `edk2`: a `profile = "windows"` TOML with no disks, only `[iso]`
//!   (installer 00:01.0, drivers 00:02.0), on the debug EDK2
//!   (`./scripts/fetch-debug-firmware.sh`). Every host INFO identification
//!   line's EDK2 text must appear verbatim in EDK2's own `PciBus` log, the
//!   boot order must name only the installer, and BDS must boot it to GRUB.
//! - `linux`: a direct kernel boot (virtio-root `Image`, `/init` replaced by
//!   [`INIT`]) with the same two slots forced onto virtio-pci. The guest
//!   reports per disc the PCI IDs, `ro`, block sizes, negotiated virtio
//!   features, the ISO 9660 identifier and volume label, the first bytes of
//!   a mid-disc and the last 2048-byte block, and whether a write was refused; each must
//!   match the host's view of the image.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use ternvale_config::{BlockDevice, VirtioTransport, VmConfig};
use ternvale_devices::{attach_blocks, last_lines, BlkPlacement, Pl011, Step};
use ternvale_vmm::pci::PciIdentity;
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, drive, init_logging, log_dir, restore_stdin, stdin_pipe, write_result,
};
use crate::firmware::expect;

const MARK: &str = "ternvale-media: ";

/// Replacement `/init`: one `ternvale-media: <dev> key=value ...` line per disc.
const INIT: &str = r#"#!/bin/busybox sh
B=/bin/busybox
$B mkdir -p /proc /sys /dev
$B mount -t proc proc /proc
$B mount -t sysfs sys /sys
$B mount -t devtmpfs dev /dev
say() { $B echo "ternvale-media: $*"; }
$B insmod /modules/virtio_blk.ko || say "insmod virtio_blk failed"
i=0
while [ ! -b /dev/vdb ] && [ "$i" -lt 100 ]; do $B sleep 0.1; i=$((i + 1)); done
for d in vda vdb; do
  s=/sys/block/$d
  p=$s/device/..
  size=$($B cat $s/size)
  last=$((size / 4 - 1))
  id=$($B dd if=/dev/$d bs=2048 skip=16 count=1 2>/dev/null | $B dd bs=1 skip=1 count=5 2>/dev/null)
  label=$($B dd if=/dev/$d bs=2048 skip=16 count=1 2>/dev/null | $B dd bs=1 skip=40 count=32 2>/dev/null | $B tr ' ' '_')
  tail=$($B dd if=/dev/$d bs=2048 skip=$last count=1 2>/dev/null | $B od -An -tx1 -N 16 | $B tr -d ' \n')
  mid=$($B dd if=/dev/$d bs=2048 skip=$((size / 8)) count=1 2>/dev/null | $B od -An -tx1 -N 16 | $B tr -d ' \n')
  $B dd if=/dev/zero of=/dev/$d bs=2048 seek=100 count=1 2>/dev/null
  wrote=$?
  say "$d pci=$($B basename $($B readlink -f $p)) vendor=$($B cat $p/vendor) device=$($B cat $p/device) subsys=$($B cat $p/subsystem_vendor):$($B cat $p/subsystem_device) class=$($B cat $p/class) rev=$($B cat $p/revision) ro=$($B cat $s/ro) lbs=$($B cat $s/queue/logical_block_size) pbs=$($B cat $s/queue/physical_block_size) size=$size features=$($B cat $s/device/features) id=$id label=$label mid=$mid tail=$tail write_exit=$wrote"
done
say done
$B poweroff -f
"#;

pub fn run() -> Result<(), String> {
    let installer = asset("installer/installer.iso", "fetch-installer-iso.sh")?;
    let drivers = asset("virtio-win/virtio-win.iso", "fetch-virtio-win.sh")?;
    let firmware = asset("firmware-debug/QEMU_EFI.fd", "fetch-debug-firmware.sh")?;
    let log_dir = log_dir();
    let guard = init_logging("windows-media", &log_dir)?;
    let host = guard.log_path().to_path_buf();
    let started = Instant::now();
    let mtimes = [modified(&installer)?, modified(&drivers)?];
    edk2(&log_dir, &host, &firmware, &installer, &drivers)?;
    linux(&log_dir, &host, &installer, &drivers)?;
    if mtimes != [modified(&installer)?, modified(&drivers)?] {
        return Err("an ISO image changed on the host".into());
    }
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "windows-media",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn asset(rel: &str, script: &str) -> Result<PathBuf, String> {
    let path = assets_root().join(rel);
    if path.is_file() {
        return Ok(path);
    }
    Err(format!(
        "missing {} (run ./scripts/{script})",
        path.display()
    ))
}

fn modified(path: &Path) -> Result<std::time::SystemTime, String> {
    std::fs::metadata(path)
        .and_then(|meta| meta.modified())
        .map_err(|err| format!("stat {}: {err}", path.display()))
}

/// Boot `vm` with its block devices on `transport`; return the serial log
/// and the PCI identity of each attached device.
fn boot(
    vm: &VmConfig,
    transport: VirtioTransport,
    steps: Vec<Step>,
    log_dir: &Path,
    host_log: &Path,
) -> Result<(String, Vec<PciIdentity>, ExitReason), String> {
    let uart = Pl011::open(&vm.serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let feed_log = vm.serial_log.clone();
    let host = host_log.to_path_buf();
    let feeder = std::thread::spawn(move || {
        let result = drive(&feed_log, &mut input, &flag, &host, steps);
        flag.store(true, Ordering::Release);
        result
    });
    let devices: Vec<BlockDevice> = vm.block_devices();
    let seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    let exit = Machine::run_with(vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let blocks = attach_blocks(attach, &devices, transport)?;
        let ids = blocks
            .devices
            .iter()
            .filter_map(|block| match block.placement {
                BlkPlacement::Pci { identity, .. } => identity,
                BlkPlacement::Mmio { .. } => None,
            });
        sink.lock()
            .map_err(|_| ternvale_vmm::MachineError::Attach("ids lock".into()))?
            .extend(ids);
        Ok(blocks.mmio)
    });
    cancel.store(true, Ordering::Release);
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&vm.serial_log).unwrap_or_default();
    write_result(log_dir, &exit, &script, &serial);
    let fail = |what: String| {
        format!(
            "{}: {what}\nlog={}\n--- serial ---\n{}",
            vm.name,
            host_log.display(),
            last_lines(&serial, 30)
        )
    };
    script.map_err(|err| fail(err.to_string()))?;
    let exit = exit.map_err(|err| fail(format!("exit error {err}")))?;
    let ids = seen
        .lock()
        .map_err(|_| "ids lock poisoned".to_string())?
        .clone();
    Ok((serial, ids, exit))
}

fn edk2(
    log_dir: &Path,
    host_log: &Path,
    firmware: &Path,
    installer: &Path,
    drivers: &Path,
) -> Result<(), String> {
    let text = format!(
        "name = \"win-media\"\nprofile = \"windows\"\nfirmware = \"{}\"\nnvram = \"{}\"\nserial_log = \"{}\"\n\n\
         [iso]\ninstaller = \"{}\"\ndrivers = \"{}\"\n",
        firmware.display(),
        log_dir.join("nvram.fd").display(),
        log_dir.join("guest-serial-edk2.log").display(),
        installer.display(),
        drivers.display(),
    );
    std::fs::write(log_dir.join("win-media.toml"), &text)
        .map_err(|err| format!("write toml: {err}"))?;
    let vm = VmConfig::from_toml(&text).map_err(|err| format!("edk2 config: {err}"))?;
    let steps = vec![
        expect("PciBus: Discovered PCI @ [00|02|00]", 120),
        expect("BdsDxe: loading", 180),
        expect("GNU GRUB", 180),
    ];
    let (serial, ids, exit) = boot(&vm, vm.virtio_transport(), steps, log_dir, host_log)?;
    if exit != ExitReason::Canceled {
        return Err(format!("edk2: exit {exit:?}, expected Canceled at GRUB"));
    }
    let serial = crate::uefi_dmem::strip(&serial);
    let host = std::fs::read_to_string(host_log).map_err(|err| format!("read host log: {err}"))?;
    let mut problems = Vec::new();
    if ids.len() != 2 {
        problems.push(format!("{} pci block devices, want 2", ids.len()));
    }
    for id in &ids {
        let found = serial
            .lines()
            .any(|line| line.trim_end().ends_with(&id.edk2_discovered()));
        let logged = host.lines().any(|line| {
            line.contains("guest-visible block device") && line.contains(&id.windows_hardware_id())
        });
        tracing::info!(target: "ternvale::boot", bdf = %id.bdf, edk2 = %id.edk2_discovered(), found, logged, "edk2 enumeration vs host identification");
        if !found || !logged {
            problems.push(format!(
                "{}: in edk2 log={found}, host INFO={logged}",
                id.bdf
            ));
        }
    }
    let order = host
        .lines()
        .find(|line| line.contains("uefi boot order: attached ISO first"));
    if !order.is_some_and(|line| {
        line.contains("order=/pci@i0cf8/scsi@1/disk@0,0") && !line.contains("scsi@2")
    }) {
        problems.push(format!(
            "boot order line {order:?}, want only the installer (scsi@1)"
        ));
    }
    let first = serial
        .lines()
        .find(|l| l.contains("BdsDxe: loading") || l.contains("BdsDxe: failed to load"));
    if !first.is_some_and(|line| line.contains("from PciRoot(0x0)/Pci(0x1,0x0)")) {
        problems.push(format!(
            "first boot attempt {first:?}, want the installer at Pci(0x1,0x0)"
        ));
    }
    report("edk2", problems)
}

fn linux(log_dir: &Path, host_log: &Path, installer: &Path, drivers: &Path) -> Result<(), String> {
    let root = assets_root().join("virtio-root");
    let read = |name: &str| {
        std::fs::read(root.join(name))
            .map_err(|err| format!("read virtio-root/{name}: {err} (run ./scripts/make-rootfs.sh)"))
    };
    let initrd = crate::cpio::append(
        &read("initramfs.cpio")?,
        &[("init", 0o755, INIT.as_bytes())],
    )?;
    let initrd_path = log_dir.join("media-initramfs.cpio");
    std::fs::write(&initrd_path, initrd).map_err(|err| format!("write initrd: {err}"))?;
    let vm = VmConfig {
        name: "win-media-linux".to_string(),
        profile: Default::default(),
        iso: Some(ternvale_config::IsoSection {
            installer: Some(installer.to_path_buf()),
            drivers: Some(drivers.to_path_buf()),
        }),
        cpus: 1,
        ram_mib: 512,
        kernel: root.join("Image"),
        initrd: Some(initrd_path),
        cmdline: String::new(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: log_dir.join("guest-serial-linux.log"),
        firmware: None,
        nvram: None,
        firmware_tables: None,
        vsock: None,
        tpm: None,
        usb: None,
        audio: None,
        shares: Vec::new(),
    };
    vm.validate()
        .map_err(|err| format!("linux config: {err}"))?;
    let steps = vec![expect("ternvale-media: done", 120)];
    let (serial, ids, exit) = boot(&vm, VirtioTransport::Pci, steps, log_dir, host_log)?;
    if exit != ExitReason::SystemOff {
        return Err(format!("linux: exit {exit:?}, expected SystemOff"));
    }
    let mut problems = Vec::new();
    for (id, image) in ids.iter().zip([installer, drivers]) {
        let pci = format!("pci=0000:{}", id.bdf);
        let Some(line) = serial
            .lines()
            .find(|l| l.contains(MARK) && l.contains(&pci))
        else {
            problems.push(format!("no guest report for {pci}"));
            continue;
        };
        tracing::info!(target: "ternvale::boot", line = %line.trim_end(), "guest view of optical disc");
        for want in expected(id, image)? {
            if !line.split_whitespace().any(|field| field == want) {
                problems.push(format!("{pci}: missing {want:?}"));
            }
        }
        let features = field(line, "features=").unwrap_or_default();
        let bit = |n: usize| features.as_bytes().get(n).copied();
        // virtio-blk RO (5) and BLK_SIZE (6) negotiated, FLUSH (9) not offered.
        if (bit(5), bit(6), bit(9)) != (Some(b'1'), Some(b'1'), Some(b'0')) {
            problems.push(format!("{pci}: features {features}"));
        }
        if field(line, "write_exit=").is_none_or(|code| code == "0") {
            problems.push(format!("{pci}: a write succeeded"));
        }
    }
    if serial.matches("2048-byte logical blocks").count() != 2 {
        problems.push("virtio_blk did not report 2048-byte logical blocks for both discs".into());
    }
    report("linux", problems)
}

/// `key=value` fields the guest line must contain for `image` at `id`.
fn expected(id: &PciIdentity, image: &Path) -> Result<Vec<String>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let mut file =
        std::fs::File::open(image).map_err(|err| format!("open {}: {err}", image.display()))?;
    let len = file.metadata().map_err(|err| err.to_string())?.len();
    let mut pvd = [0u8; 2048];
    file.seek(SeekFrom::Start(16 * 2048))
        .and_then(|_| file.read_exact(&mut pvd))
        .map_err(|err| err.to_string())?;
    let mut sample = |block: u64| -> Result<String, String> {
        let mut bytes = [0u8; 16];
        file.seek(SeekFrom::Start(block * 2048))
            .and_then(|_| file.read_exact(&mut bytes))
            .map_err(|err| format!("read {} block {block}: {err}", image.display()))?;
        Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
    };
    // The guest skips `size / 8` 2048-byte blocks, where size counts 512-byte sectors.
    let mid = sample(len / 512 / 8)?;
    if mid.bytes().all(|b| b == b'0') {
        return Err(format!(
            "{}: middle sample is all zeros, so it proves nothing; sample another block",
            image.display()
        ));
    }
    let tail = sample(len / 2048 - 1)?;
    let label = String::from_utf8_lossy(&pvd[40..72]).replace(' ', "_");
    Ok(vec![
        format!("vendor={:#06x}", id.vendor_id),
        format!("device={:#06x}", id.device_id),
        format!(
            "subsys={:#06x}:{:#06x}",
            id.subsystem_vendor_id, id.subsystem_id
        ),
        format!("class={:#08x}", id.class_code),
        format!("rev={:#04x}", id.revision),
        "ro=1".into(),
        "lbs=2048".into(),
        "pbs=2048".into(),
        format!("size={}", len / 512),
        format!("id={}", String::from_utf8_lossy(&pvd[1..6])),
        format!("label={label}"),
        format!("mid={mid}"),
        format!("tail={tail}"),
    ])
}

fn field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split_whitespace().find_map(|f| f.strip_prefix(key))
}

fn report(label: &str, problems: Vec<String>) -> Result<(), String> {
    if problems.is_empty() {
        tracing::info!(target: "ternvale::boot", label, "windows media checks passed");
        return Ok(());
    }
    tracing::error!(target: "ternvale::boot", label, problems = %problems.join("; "), "windows media checks failed");
    Err(format!("{label}: {}", problems.join("; ")))
}
