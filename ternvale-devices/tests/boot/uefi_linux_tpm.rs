//! Linux guest on the real TPM: EDK2 with `firmware_tables = "acpi"` and
//! `[tpm] backend = "swtpm"` boots Linux twice on one NVRAM file, so both
//! boots share swtpm's state in `nvram.fd.swtpm`.
//!
//! Assets come from `./scripts/make-tpm-assets.sh`: Ubuntu's arm64 kernel
//! (`tpm_crb` built in; Alpine's kernels have no CRB driver) and an Alpine
//! initramfs with tpm2-tools. Both go on a FAT16 ESP on virtio-pci. The
//! replacement `/init` ([`INIT`]) finds the TPM through `\_SB.TPM2`
//! (`MSFT0101`, bound by `tpm_crb`) and, over `/dev/tpmrm0`, runs
//! `tpm2_getrandom` twice, reads `TPM2_PT_MANUFACTURER`, creates the ECC EK,
//! and keeps a 32-byte NV index: boot 1 defines it and writes TPM random
//! bytes, boot 2 must find and read back the same bytes. It also reports the
//! size of EDK2's TCG2 event log and PCRs 0 and 7 (logged, not checked). Under ACPI there is
//! no console device, so the script writes to `/dev/kmsg` (earlycon carries
//! it to the serial log), then powers off. Each boot's crash-report TPM
//! command history must keep EDK2's `TPM2_Startup` bytes and redact the
//! guest's `TPM2_GetRandom`. A third boot with `[tpm] enabled = false` must
//! show Linux no TPM at all and leave swtpm's state untouched.
//! [`crate::tpm_guest_checks`] holds the checks.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use ternvale_config::{Disk, FirmwareTables, TpmBackendKind, TpmSection, VmConfig};
use ternvale_devices::{attach_disks_pci, last_lines, Pl011, Step};
use ternvale_vmm::{ExitReason, Machine, VmControl};

use crate::common::{
    assets_root, drive, init_logging_at, log_dir, restore_stdin, stdin_pipe, write_result,
};
use crate::firmware::{check_banner, expect, to_shell};
use crate::tpm_guest_checks::{
    check_boot, check_disabled, check_history, check_host_log, check_restart, Guest, DONE,
};
use crate::uefi_linux::{line, INITRD, KERNEL};

const CPUS: u32 = 2;

/// Replacement `/init`. Every result is one `ternvale-tpm: <what> k=v …`
/// line on `/dev/kmsg` (`printk.devkmsg=on` lifts its rate limit).
const INIT: &str = r#"#!/bin/busybox sh
B=/bin/busybox
$B mkdir -p /proc /sys /dev /tmp
$B mount -t proc proc /proc
$B mount -t sysfs sys /sys
$B mount -t devtmpfs dev /dev
$B mount -t tmpfs tmp /tmp
exec </dev/null >/dev/kmsg 2>&1
say() { $B echo "ternvale-tpm: $*"; }
export TPM2TOOLS_TCTI=device:/dev/tpmrm0
i=0
while [ ! -c /dev/tpmrm0 ] && [ "$i" -lt 100 ]; do $B sleep 0.1; i=$((i + 1)); done
drv=$($B readlink /sys/class/tpm/tpm0/device/driver)
say "probe acpi_tpm2=$([ -r /sys/firmware/acpi/tables/TPM2 ] && echo y || echo n) tpmrm0=$([ -c /dev/tpmrm0 ] && echo y || echo n) driver=${drv##*/} major=$($B cat /sys/class/tpm/tpm0/tpm_version_major) hwrng=$($B cat /sys/class/misc/hw_random/rng_current)"
$B mount -t securityfs securityfs /sys/kernel/security
say "eventlog bytes=$($B wc -c </sys/kernel/security/tpm0/binary_bios_measurements 2>/dev/null)"
r=$(/usr/bin/tpm2_getrandom --hex 32 2>>/tmp/err); rc=$?; say "random1 rc=$rc hex=$r"
r=$(/usr/bin/tpm2_getrandom --hex 32 2>>/tmp/err); rc=$?; say "random2 rc=$rc hex=$r"
m=$(/usr/bin/tpm2_getcap properties-fixed 2>>/tmp/err | $B grep -A1 'TPM2_PT_MANUFACTURER:' | $B sed -n 's/^ *raw: *//p')
say "manufacturer raw=$m"
/usr/bin/tpm2_createek -c /tmp/ek.ctx -G ecc -u /tmp/ek.pub >/dev/null 2>>/tmp/err; rc=$?
say "ek rc=$rc hex=$($B od -An -v -tx1 /tmp/ek.pub | $B tr -d ' \n')"
NV=0x1500016
if /usr/bin/tpm2_nvreadpublic $NV >/dev/null 2>&1; then
  state=existing; rc=0
else
  state=new
  /usr/bin/tpm2_getrandom -o /tmp/nv.bin 32 2>>/tmp/err \
    && /usr/bin/tpm2_nvdefine $NV -C o -s 32 -a "ownerread|ownerwrite" >/dev/null 2>>/tmp/err \
    && /usr/bin/tpm2_nvwrite $NV -C o -i /tmp/nv.bin 2>>/tmp/err
  rc=$?
fi
v=$(/usr/bin/tpm2_nvread $NV -C o -s 32 2>>/tmp/err | $B od -An -v -tx1 | $B tr -d ' \n')
say "nv index=$NV state=$state rc=$rc hex=$v"
/usr/bin/tpm2_pcrread sha256:0,7 2>>/tmp/err | while IFS= read -r l; do say "pcr $l"; done
while IFS= read -r l; do say "stderr $l"; done </tmp/err
say done
$B sync
$B poweroff -f
"#;

pub fn run() -> Result<(), String> {
    let log_dir = log_dir();
    // TRACE on ternvale::tpm with secrets unredacted records every CRB
    // response, so the check can find the guest's random bytes in what swtpm
    // returned. The command history stays redacted regardless.
    std::env::set_var(ternvale_tpm::LOG_SECRETS_ENV, "1");
    let base = std::env::var("TERNVALE_BOOT_LOG").unwrap_or_else(|_| "info".to_string());
    let guard = init_logging_at(
        "uefi-linux-tpm",
        &log_dir,
        &format!("{base},ternvale::tpm=trace"),
    )?;
    let host = guard.log_path().to_path_buf();
    let assets = assets_root().join("tpm-linux");
    let read = |name: &str| {
        let path = assets.join(name);
        std::fs::read(&path).map_err(|err| {
            format!(
                "read {}: {err} (run ./scripts/make-tpm-assets.sh)",
                path.display()
            )
        })
    };
    let kernel = read("Image")?;
    let initrd = crate::cpio::append(
        &read("initramfs.cpio")?,
        &[("init", 0o755, INIT.as_bytes())],
    )?;
    let esp = log_dir.join("esp.img");
    crate::fat::write_fat16_mbr(&esp, &[(KERNEL, &kernel), (INITRD, &initrd)])?;
    let nvram = log_dir.join("nvram.fd");
    let state = log_dir.join("nvram.fd.swtpm");
    for old in [&nvram, &state] {
        if old.exists() {
            std::fs::remove_file(old).map_err(|err| format!("remove {}: {err}", old.display()))?;
        }
    }
    let started = Instant::now();
    let mut guests = Vec::new();
    for boot in 1..=2 {
        let (serial, history) = boot_once(boot, true, &esp, &nvram, &log_dir, &host)?;
        let history = history.ok_or("the vm recorded no tpm command history")?;
        let guest = Guest::parse(&serial);
        check_boot(boot, &guest)?;
        check_history(boot, &guest, &history)?;
        guests.push(guest);
    }
    let state_bytes =
        std::fs::read(&state).map_err(|err| format!("read {}: {err}", state.display()))?;
    let (serial, history) = boot_once(3, false, &esp, &nvram, &log_dir, &host)?;
    check_disabled(
        &Guest::parse(&serial),
        history.as_deref(),
        &state,
        &state_bytes,
    )?;
    let host_ek = crate::firmware_tpm::create_primary(&state, ek_template())?;
    check_restart(&guests[0], &guests[1], &host_ek)?;
    check_host_log(&host, &guests)?;
    tracing::info!(
        target: "ternvale::boot",
        boot_ms = started.elapsed().as_millis() as u64,
        scenario = "uefi-linux-tpm",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

/// TCG EK Credential Profile 2.0 template L-2 (ECC NIST P-256), which
/// `tpm2_createek -G ecc` uses: the EK policy (PolicySecret on the
/// endorsement hierarchy), AES-128-CFB, and 32-byte zero `unique` x and y.
/// TODO(verify): the profile section number (B.3.4 in revision 14).
fn ek_template() -> Vec<u8> {
    let zeros = "00".repeat(32);
    crate::firmware_tpm::unhex(
        &[
            "0023",
            "000b",
            "000300b2",
            "0020",
            "837197674484b3f81a90cc8d46a5d724fd52d76e06520b64f2a1da1b331469aa",
            "0006",
            "0080",
            "0043",
            "0010",
            "0003",
            "0010",
            "0020",
            &zeros,
            "0020",
            &zeros,
        ]
        .concat(),
    )
}

/// One boot; `tpm` false boots with `[tpm] enabled = false`. Returns the
/// serial log and the rendered TPM command history (`None` without a TPM).
fn boot_once(
    boot: u32,
    tpm: bool,
    esp: &Path,
    nvram: &Path,
    log_dir: &Path,
    host_log: &Path,
) -> Result<(String, Option<String>), String> {
    let serial_log = log_dir.join(format!("guest-serial-{boot}.log"));
    let vm = VmConfig {
        name: "uefi-linux-tpm".to_string(),
        cpus: CPUS,
        ram_mib: 1024,
        kernel: Default::default(),
        initrd: None,
        cmdline: String::new(),
        boot_disk: false,
        disks: vec![Disk {
            path: esp.to_path_buf(),
            read_only: true,
        }],
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: Some(crate::common::firmware()),
        nvram: Some(nvram.to_path_buf()),
        firmware_tables: Some(FirmwareTables::Acpi),
        vsock: None,
        tpm: Some(TpmSection {
            backend: if tpm {
                TpmBackendKind::Swtpm
            } else {
                TpmBackendKind::Auto
            },
            swtpm: None,
            enabled: tpm,
        }),
    };
    tracing::info!(target: "ternvale::boot", boot, tpm, serial = %serial_log.display(), "uefi linux tpm boot");
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let (feed_log, host) = (serial_log.clone(), host_log.to_path_buf());
    let feeder = std::thread::spawn(move || drive(&feed_log, &mut input, &flag, &host, steps()));
    let attach = vec![(esp.to_path_buf(), true)];
    let control = Arc::new(VmControl::new(&vm.name, vm.cpus));
    let stopper = stop_on_cancel(&control, &cancel);
    let exit = Machine::run_controlled(&vm, Box::new(uart), &control, move |a| {
        attach_disks_pci(a, &attach).map(|_| Vec::new())
    });
    cancel.store(true, Ordering::Release);
    let script = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    if stopper.join().is_err() {
        tracing::warn!(target: "ternvale::boot", boot, "stop watcher panicked");
    }
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(log_dir, &exit, &script, &serial);
    let history = control.diagnostics().tpm().map(|h| h.render());
    let history_file = log_dir.join(format!("tpm-commands-{boot}.txt"));
    std::fs::write(&history_file, history.as_deref().unwrap_or(""))
        .map_err(|err| format!("write {}: {err}", history_file.display()))?;
    script?;
    if !matches!(exit, Ok(ExitReason::SystemOff)) {
        return Err(format!(
            "boot {boot}: exit={exit:?} (expected SystemOff)\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 30)
        ));
    }
    check_banner(&serial_log)?;
    Ok((serial, history))
}

/// Force-stop the VM `control` runs once `cancel` is set (the serial driver
/// timed out); returns when the VM has finished either way.
fn stop_on_cancel(
    control: &Arc<VmControl>,
    cancel: &Arc<AtomicBool>,
) -> std::thread::JoinHandle<()> {
    let (control, cancel) = (Arc::clone(control), Arc::clone(cancel));
    std::thread::spawn(move || {
        while !cancel.load(Ordering::Acquire) {
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        if let Err(error) = control.request_stop(true) {
            tracing::debug!(target: "ternvale::boot", error = %error, "vm already finished");
        }
    })
}

fn steps() -> Vec<Step> {
    let mut steps = to_shell(true);
    let command = format!(
        "fs0:\\{KERNEL} initrd=\\{INITRD} {} printk.devkmsg=on",
        ternvale_vmm::DEFAULT_CMDLINE
    );
    steps.extend(line(&command, b"\r"));
    steps.push(expect("EFI stub: Booting Linux Kernel", 120));
    steps.push(expect(DONE, 180));
    steps
}
