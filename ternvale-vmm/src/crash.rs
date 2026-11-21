//! Guest panic / watchdog policy, RAM dumps, and unclean-shutdown markers.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use ternvale_config::{OnPanic, OnWatchdog, ReliabilitySection};

use crate::control::VmControl;
use crate::lockwatch;
use crate::memory::GuestMemory;
use crate::smp::CpuPower;
use crate::vcpu::ExitReason;

/// Linux `include/uapi/linux/pvpanic.h` / QEMU `hw/misc/pvpanic.h`: bit 0
/// `PVPANIC_PANICKED`, bit 1 `PVPANIC_CRASHLOADED`, bit 2 `PVPANIC_SHUTDOWN`.
pub const PVPANIC_PANICKED: u8 = 1 << 0;
/// Guest loaded a crash kernel (kdump).
pub const PVPANIC_CRASHLOADED: u8 = 1 << 1;
/// Orderly shutdown notification (not a kernel panic).
pub const PVPANIC_SHUTDOWN: u8 = 1 << 2;
/// Events this host handles (write-1 to report).
pub const PVPANIC_EVENTS: u8 = PVPANIC_PANICKED | PVPANIC_CRASHLOADED | PVPANIC_SHUTDOWN;
/// Red Hat / QEMU PCI vendor (`pci.ids`).
pub const PVPANIC_PCI_VENDOR: u16 = 0x1b36;
/// QEMU `pvpanic-pci` device ID.
pub const PVPANIC_PCI_DEVICE: u16 = 0x0011;
/// ACPI `_HID` (`drivers/misc/pvpanic/pvpanic-acpi.c`: `QEMU0001`).
pub const PVPANIC_ACPI_HID: &str = "QEMU0001";
/// DT compatible (`drivers/misc/pvpanic/pvpanic-mmio.c`).
pub const PVPANIC_DT_COMPAT: &str = "qemu,pvpanic-mmio";

/// Versioned RAM dump magic (`TVRD`).
pub const RAM_DUMP_MAGIC: &[u8; 4] = b"TVRD";
/// RAM dump format version.
pub const RAM_DUMP_VERSION: u32 = 1;
/// Dump file name in the VM bundle.
pub const RAM_DUMP_FILE: &str = "memory.dump";
/// Sidecar warning (RAM can contain secrets).
pub const RAM_DUMP_WARNING_FILE: &str = "memory.dump.WARNING.txt";
/// Unclean-shutdown marker magic (`TVUC`).
pub const UNCLEAN_MAGIC: &[u8; 4] = b"TVUC";
/// Marker format version.
pub const UNCLEAN_VERSION: u32 = 1;
/// Marker file next to the VM config / disks.
pub const UNCLEAN_FILE: &str = "unclean-shutdown";

/// Privacy warning stored beside a RAM dump (and shown in the crash UI).
pub const RAM_DUMP_PRIVACY_WARNING: &str = "\
This file is a guest RAM dump. It can contain passwords, encryption keys, \
browser cookies, and other secrets from the virtual machine. Do not share it \
unless you have redacted or you accept that risk. The dump is size-capped \
by reliability.dump_ram_max_mib.
";

/// Why the guest asked the host to act.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CrashKind {
    /// `pvpanic` write. `events` is the guest byte (bits from the spec).
    Panic { events: u8 },
    /// SBSA generic watchdog WS1 (reset window).
    Watchdog,
}

/// Shared sink: devices may fire before [`CrashRuntime`] is bound.
pub struct CrashHub {
    runtime: Mutex<Option<Arc<CrashRuntime>>>,
    pending: Mutex<Vec<CrashKind>>,
}

/// Bound policy + VM handles.
pub struct CrashRuntime {
    reliability: ReliabilitySection,
    control: Arc<VmControl>,
    power: Arc<CpuPower>,
    memory: Arc<Mutex<GuestMemory>>,
    dump_dir: PathBuf,
    ram_base: u64,
    ram_size: u64,
}

impl CrashHub {
    /// Empty hub.
    #[tracing::instrument(level = "debug", target = "ternvale::pvpanic", skip_all)]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            runtime: Mutex::new(None),
            pending: Mutex::new(Vec::new()),
        })
    }

    /// Attach the live VM. Flushes events that arrived during attach.
    #[tracing::instrument(level = "debug", target = "ternvale::pvpanic", skip_all)]
    pub fn bind(&self, runtime: Arc<CrashRuntime>) {
        let pending = {
            let mut slot = lockwatch::lock(&self.runtime, "crash-hub");
            *slot = Some(Arc::clone(&runtime));
            lockwatch::lock(&self.pending, "crash-pending")
                .drain(..)
                .collect::<Vec<_>>()
        };
        for kind in pending {
            runtime.handle(kind);
        }
    }

    /// Guest panic or watchdog. Logged at ERROR with the reason.
    #[tracing::instrument(level = "debug", target = "ternvale::pvpanic", skip_all)]
    pub fn report(&self, kind: CrashKind) {
        let runtime = lockwatch::lock(&self.runtime, "crash-hub").clone();
        match runtime {
            Some(runtime) => runtime.handle(kind),
            None => {
                tracing::warn!(
                    target: "ternvale::pvpanic",
                    ?kind,
                    "crash event before runtime bind; queued"
                );
                lockwatch::lock(&self.pending, "crash-pending").push(kind);
            }
        }
    }
}

impl CrashRuntime {
    /// Policy plus dump/pause/stop handles.
    #[allow(clippy::too_many_arguments)]
    #[tracing::instrument(level = "debug", target = "ternvale::pvpanic", skip_all)]
    pub fn new(
        reliability: ReliabilitySection,
        control: Arc<VmControl>,
        power: Arc<CpuPower>,
        memory: Arc<Mutex<GuestMemory>>,
        dump_dir: PathBuf,
        ram_base: u64,
        ram_size: u64,
    ) -> Self {
        Self {
            reliability,
            control,
            power,
            memory,
            dump_dir,
            ram_base,
            ram_size,
        }
    }

    /// Apply [`ReliabilitySection`] for `kind`.
    #[tracing::instrument(level = "debug", target = "ternvale::pvpanic", skip_all, fields(?kind))]
    pub fn handle(&self, kind: CrashKind) {
        match kind {
            CrashKind::Panic { events } => {
                tracing::error!(
                    target: "ternvale::pvpanic",
                    events = format!("{events:#04x}"),
                    panicked = events & PVPANIC_PANICKED != 0,
                    crashloaded = events & PVPANIC_CRASHLOADED != 0,
                    shutdown = events & PVPANIC_SHUTDOWN != 0,
                    policy = self.reliability.on_panic.as_str(),
                    "guest panic notification"
                );
                if events & PVPANIC_SHUTDOWN != 0
                    && events & (PVPANIC_PANICKED | PVPANIC_CRASHLOADED) == 0
                {
                    self.power.request_stop(ExitReason::SystemOff);
                    return;
                }
                self.apply_panic();
            }
            CrashKind::Watchdog => {
                tracing::error!(
                    target: "ternvale::watchdog",
                    policy = self.reliability.on_watchdog.as_str(),
                    "SBSA generic watchdog WS1 (guest failed to refresh)"
                );
                self.apply_watchdog();
            }
        }
    }

    fn apply_panic(&self) {
        match self.reliability.on_panic {
            OnPanic::Pause => self.pause_later(),
            OnPanic::Restart => self.power.request_stop(ExitReason::SystemReset),
            OnPanic::Poweroff => self.power.request_stop(ExitReason::SystemOff),
            OnPanic::DumpAndPause => {
                if let Err(error) = self.dump_ram() {
                    tracing::error!(
                        target: "ternvale::pvpanic",
                        error = %error,
                        "guest RAM dump failed; pausing anyway"
                    );
                }
                self.pause_later();
            }
        }
    }

    fn apply_watchdog(&self) {
        match self.reliability.on_watchdog {
            OnWatchdog::Reset => self.power.request_stop(ExitReason::SystemReset),
            OnWatchdog::Poweroff => self.power.request_stop(ExitReason::SystemOff),
            OnWatchdog::Pause => self.pause_later(),
        }
    }

    fn pause_later(&self) {
        let control = Arc::clone(&self.control);
        if let Err(error) = std::thread::Builder::new()
            .name("crash-pause".into())
            .spawn(move || {
                if let Err(error) = control.pause(Duration::from_secs(15)) {
                    tracing::error!(
                        target: "ternvale::pvpanic",
                        error = %error,
                        "pause after guest crash failed"
                    );
                }
            })
        {
            tracing::error!(
                target: "ternvale::pvpanic",
                error = %error,
                "could not spawn crash-pause thread"
            );
        }
    }

    fn dump_ram(&self) -> Result<PathBuf, std::io::Error> {
        let cap = self
            .reliability
            .dump_ram_max_mib
            .saturating_mul(1024 * 1024)
            .min(self.ram_size);
        fs::create_dir_all(&self.dump_dir)?;
        let path = self.dump_dir.join(RAM_DUMP_FILE);
        let warn = self.dump_dir.join(RAM_DUMP_WARNING_FILE);
        fs::write(&warn, RAM_DUMP_PRIVACY_WARNING)?;
        let memory = lockwatch::lock(&self.memory, "guest-memory");
        write_ram_dump(&path, &memory, self.ram_base, self.ram_size, cap)?;
        drop(memory);
        tracing::error!(
            target: "ternvale::pvpanic",
            path = %path.display(),
            cap,
            ram_size = self.ram_size,
            "wrote capped guest RAM dump (privacy: RAM can contain secrets)"
        );
        Ok(path)
    }
}

/// Write a versioned dump of the first `cap` bytes of guest RAM.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::pvpanic",
    skip_all,
    fields(path = %path.display(), cap)
)]
pub fn write_ram_dump(
    path: &Path,
    memory: &GuestMemory,
    ram_base: u64,
    ram_size: u64,
    cap: u64,
) -> Result<(), std::io::Error> {
    let dumped = cap.min(ram_size);
    let mut file = File::create(path)?;
    file.write_all(RAM_DUMP_MAGIC)?;
    file.write_all(&RAM_DUMP_VERSION.to_le_bytes())?;
    file.write_all(&ram_base.to_le_bytes())?;
    file.write_all(&ram_size.to_le_bytes())?;
    file.write_all(&dumped.to_le_bytes())?;
    file.write_all(&cap.to_le_bytes())?;
    const CHUNK: usize = 1024 * 1024;
    let mut off = 0u64;
    while off < dumped {
        let n = ((dumped - off) as usize).min(CHUNK);
        let bytes = memory
            .copy_region(ram_base + off, n as u64)
            .map_err(|error| std::io::Error::other(error.to_string()))?;
        file.write_all(&bytes)?;
        off += n as u64;
    }
    Ok(())
}

/// Reject unknown dump versions.
#[tracing::instrument(level = "debug", target = "ternvale::pvpanic", skip_all)]
pub fn parse_ram_dump_header(bytes: &[u8]) -> Result<(u64, u64, u64, u64), String> {
    if bytes.len() < 40 {
        return Err("truncated RAM dump".into());
    }
    if &bytes[0..4] != RAM_DUMP_MAGIC {
        return Err("bad RAM dump magic".into());
    }
    let version = u32::from_le_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| "truncated RAM dump version")?,
    );
    if version != RAM_DUMP_VERSION {
        return Err(format!("unsupported RAM dump version {version}"));
    }
    let ram_base = u64::from_le_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| "truncated RAM dump ram_base")?,
    );
    let ram_size = u64::from_le_bytes(
        bytes[16..24]
            .try_into()
            .map_err(|_| "truncated RAM dump ram_size")?,
    );
    let dumped = u64::from_le_bytes(
        bytes[24..32]
            .try_into()
            .map_err(|_| "truncated RAM dump length")?,
    );
    let cap = u64::from_le_bytes(
        bytes[32..40]
            .try_into()
            .map_err(|_| "truncated RAM dump cap")?,
    );
    Ok((ram_base, ram_size, dumped, cap))
}

/// Write `unclean-shutdown` (versioned) after a VMM-process crash.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::watchdog",
    skip_all,
    fields(dir = %dir.display())
)]
pub fn mark_unclean_shutdown(dir: &Path, reason: &str) -> Result<PathBuf, std::io::Error> {
    fs::create_dir_all(dir)?;
    let path = dir.join(UNCLEAN_FILE);
    let ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    let mut body = Vec::new();
    body.extend_from_slice(UNCLEAN_MAGIC);
    body.extend_from_slice(&UNCLEAN_VERSION.to_le_bytes());
    body.extend_from_slice(&ms.to_le_bytes());
    let reason_bytes = reason.as_bytes();
    let len = u32::try_from(reason_bytes.len()).unwrap_or(u32::MAX);
    body.extend_from_slice(&len.to_le_bytes());
    body.extend_from_slice(&reason_bytes[..len as usize]);
    fs::write(&path, body)?;
    tracing::error!(
        target: "ternvale::watchdog",
        path = %path.display(),
        reason,
        "disk marked unclean-shutdown"
    );
    Ok(path)
}

/// Parse [`UNCLEAN_FILE`]. Unknown versions are errors.
#[tracing::instrument(level = "debug", target = "ternvale::watchdog", skip_all)]
pub fn parse_unclean_shutdown(bytes: &[u8]) -> Result<(u64, String), String> {
    if bytes.len() < 20 {
        return Err("truncated unclean-shutdown marker".into());
    }
    if &bytes[0..4] != UNCLEAN_MAGIC {
        return Err("bad unclean-shutdown magic".into());
    }
    let version = u32::from_le_bytes(
        bytes[4..8]
            .try_into()
            .map_err(|_| "truncated unclean-shutdown version")?,
    );
    if version != UNCLEAN_VERSION {
        return Err(format!("unsupported unclean-shutdown version {version}"));
    }
    let ms = u64::from_le_bytes(
        bytes[8..16]
            .try_into()
            .map_err(|_| "truncated unclean-shutdown timestamp")?,
    );
    let len = u32::from_le_bytes(
        bytes[16..20]
            .try_into()
            .map_err(|_| "truncated unclean-shutdown length")?,
    ) as usize;
    let rest = bytes.get(20..).unwrap_or(&[]);
    if rest.len() < len {
        return Err("truncated unclean-shutdown reason".into());
    }
    let reason = String::from_utf8_lossy(&rest[..len]).into_owned();
    Ok((ms, reason))
}

#[cfg(test)]
#[path = "crash_quality.rs"]
mod quality;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::HOST_PAGE_SIZE;

    #[test]
    fn dump_is_capped_and_header_rejects_unknown_version() {
        let dir = std::env::temp_dir().join(format!("tv-dump-{}", std::process::id()));
        let _ = fs::create_dir_all(&dir);
        let mut memory = GuestMemory::new().expect("mem");
        memory.add_region(0, HOST_PAGE_SIZE * 4).expect("ram");
        memory
            .write_bytes(0, &[0xABu8; HOST_PAGE_SIZE as usize])
            .expect("fill");
        let path = dir.join(RAM_DUMP_FILE);
        write_ram_dump(&path, &memory, 0, HOST_PAGE_SIZE * 4, HOST_PAGE_SIZE).expect("dump");
        let bytes = fs::read(&path).expect("read");
        let (base, size, dumped, cap) = parse_ram_dump_header(&bytes).expect("hdr");
        assert_eq!(base, 0);
        assert_eq!(size, HOST_PAGE_SIZE * 4);
        assert_eq!(dumped, HOST_PAGE_SIZE);
        assert_eq!(cap, HOST_PAGE_SIZE);
        assert_eq!(bytes.len(), 40 + HOST_PAGE_SIZE as usize);
        let mut bad = bytes.clone();
        bad[4..8].copy_from_slice(&99u32.to_le_bytes());
        let err = parse_ram_dump_header(&bad).expect_err("ver");
        assert!(err.contains("unsupported"), "{err}");
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn unclean_marker_round_trips_and_rejects_unknown_version() {
        let dir = std::env::temp_dir().join(format!("tv-unclean-{}", std::process::id()));
        let path = mark_unclean_shutdown(&dir, "vmm process died").expect("write");
        let bytes = fs::read(&path).expect("read");
        let (_, reason) = parse_unclean_shutdown(&bytes).expect("parse");
        assert_eq!(reason, "vmm process died");
        let mut bad = bytes;
        bad[4..8].copy_from_slice(&2u32.to_le_bytes());
        assert!(parse_unclean_shutdown(&bad)
            .unwrap_err()
            .contains("unsupported"));
        let _ = fs::remove_dir_all(&dir);
    }
}
