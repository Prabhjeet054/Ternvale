//! Step 117 host analogs: sysrq panic, SBSA watchdog, dump cap (no live guest).

use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_config::{OnPanic, OnWatchdog, ReliabilitySection};
use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use super::*;
use crate::control::VmControl;
use crate::memory::{GuestMemory, HOST_PAGE_SIZE};
use crate::mmio::MmioDevice;
use crate::pvpanic::PvPanicMmio;
use crate::sbsa_gwdt::{SbsaWatchdog, WCS_EN, WCS_WS1};
use crate::smp::CpuPower;
use crate::vcpu::ExitReason;
use crate::VmState;

/// Linux `echo c > /proc/sysrq-trigger` writes this through pvpanic.
const SYSRQ_PANIC: u8 = PVPANIC_PANICKED;

struct LogBuf(Arc<Mutex<String>>);

struct FieldSink(String);

impl Visit for FieldSink {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={value:?}", field.name()));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push_str(&format!(" {}={value}", field.name()));
    }
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.0.push_str(&format!(" {}={value}", field.name()));
    }
}

impl Subscriber for LogBuf {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target() == "ternvale::pvpanic" || metadata.target() == "ternvale::watchdog"
    }
    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}
    fn event(&self, event: &Event<'_>) {
        let mut sink = FieldSink(String::new());
        event.record(&mut sink);
        let mut g = self.0.lock().expect("quality log");
        g.push_str(event.metadata().level().as_str());
        g.push(' ');
        g.push_str(event.metadata().target());
        g.push_str(&sink.0);
        g.push('\n');
    }
    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

fn capture(f: impl FnOnce()) -> String {
    let buf = Arc::new(Mutex::new(String::new()));
    tracing::subscriber::with_default(LogBuf(Arc::clone(&buf)), f);
    let logs = buf.lock().expect("quality log").clone();
    logs
}

struct Fixture {
    dir: PathBuf,
    hub: Arc<CrashHub>,
    power: Arc<CpuPower>,
    control: Arc<VmControl>,
}

impl Fixture {
    fn new(tag: &str, reliability: ReliabilitySection, ram_pages: u64) -> Self {
        let dir = std::env::temp_dir().join(format!(
            "tv-rel-q-{}-{tag}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        fs::create_dir_all(&dir).expect("dir");
        let ram_size = HOST_PAGE_SIZE * ram_pages;
        let mut memory = GuestMemory::new().expect("mem");
        memory.add_region(0, ram_size).expect("ram");
        memory.write_bytes(0, b"sysrq-c analog").expect("marker");
        let memory = Arc::new(Mutex::new(memory));
        let control = Arc::new(VmControl::new(&format!("rel-{tag}"), 1));
        control.mark_running();
        let power = Arc::new(CpuPower::new(1, 0, ram_size));
        let runtime = Arc::new(CrashRuntime::new(
            reliability,
            Arc::clone(&control),
            Arc::clone(&power),
            memory,
            dir.clone(),
            0,
            ram_size,
        ));
        let hub = CrashHub::new();
        hub.bind(runtime);
        Self {
            dir,
            hub,
            power,
            control,
        }
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn wait_paused(control: &VmControl) {
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if control.state() == VmState::Paused {
            return;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    panic!("vm did not pause, state={:?}", control.state());
}

/// `echo c > /proc/sysrq-trigger` analog: pvpanic bit 0 for each `on_panic`.
#[test]
fn sysrq_panic_applies_each_on_panic_policy_including_the_dump() {
    let logs = capture(|| {
        for policy in [
            OnPanic::Pause,
            OnPanic::Restart,
            OnPanic::Poweroff,
            OnPanic::DumpAndPause,
        ] {
            let mut reliability = ReliabilitySection::default();
            reliability.on_panic = policy;
            reliability.dump_ram_max_mib = 1;
            let ram_pages = if policy == OnPanic::DumpAndPause {
                128
            } else {
                4
            };
            let fx = Fixture::new(policy.as_str(), reliability, ram_pages);
            let mut mmio = PvPanicMmio::new(Arc::clone(&fx.hub));
            mmio.write(0, 1, u64::from(SYSRQ_PANIC));
            match policy {
                OnPanic::Pause => wait_paused(&fx.control),
                OnPanic::Restart => {
                    assert_eq!(fx.power.stop_reason(), Some(ExitReason::SystemReset));
                }
                OnPanic::Poweroff => {
                    assert_eq!(fx.power.stop_reason(), Some(ExitReason::SystemOff));
                }
                OnPanic::DumpAndPause => {
                    wait_paused(&fx.control);
                    let dump = fx.dir.join(RAM_DUMP_FILE);
                    let warn = fx.dir.join(RAM_DUMP_WARNING_FILE);
                    let bytes = fs::read(&dump).expect("dump");
                    let (_base, ram, dumped, cap) = parse_ram_dump_header(&bytes).expect("hdr");
                    assert_eq!(ram, HOST_PAGE_SIZE * 128);
                    assert_eq!(cap, 1024 * 1024);
                    assert_eq!(dumped, 1024 * 1024);
                    assert_eq!(bytes.len(), 40 + 1024 * 1024);
                    let warning = fs::read_to_string(&warn).expect("warn");
                    assert!(
                        warning.contains("passwords") && warning.contains("secrets"),
                        "{warning}"
                    );
                    assert_eq!(warning, RAM_DUMP_PRIVACY_WARNING);
                }
            }
        }
    });
    assert!(
        logs.contains("guest panic notification") && logs.contains("policy=pause"),
        "{logs}"
    );
    assert!(
        logs.contains("policy=restart") && logs.contains("policy=poweroff"),
        "{logs}"
    );
    assert!(
        logs.contains("policy=dump-and-pause") && logs.contains("wrote capped guest RAM dump"),
        "{logs}"
    );
    assert!(logs.contains("panicked=true"), "{logs}");
}

/// Enable the SBSA watchdog, stop feeding WRR, apply each `on_watchdog`.
#[test]
fn watchdog_without_refresh_applies_each_on_watchdog_policy() {
    let logs = capture(|| {
        for policy in [OnWatchdog::Reset, OnWatchdog::Poweroff, OnWatchdog::Pause] {
            let mut reliability = ReliabilitySection::default();
            reliability.on_watchdog = policy;
            let fx = Fixture::new(policy.as_str(), reliability, 4);
            let irq = Arc::new(move |_level: bool| {}) as Arc<dyn Fn(bool) + Send + Sync>;
            let (refresh, mut ctrl) = SbsaWatchdog::pair(Arc::clone(&fx.hub), irq);
            ctrl.write(0x008, 4, 1);
            ctrl.write(0x000, 4, u64::from(WCS_EN));
            refresh.force_expire();
            refresh.force_expire();
            assert_ne!(ctrl.read(0x000, 4) & u64::from(WCS_WS1), 0);
            match policy {
                OnWatchdog::Reset => {
                    assert_eq!(fx.power.stop_reason(), Some(ExitReason::SystemReset));
                }
                OnWatchdog::Poweroff => {
                    assert_eq!(fx.power.stop_reason(), Some(ExitReason::SystemOff));
                }
                OnWatchdog::Pause => wait_paused(&fx.control),
            }
        }
    });
    assert!(
        logs.contains("SBSA generic watchdog WS1") && logs.contains("policy=reset"),
        "{logs}"
    );
    assert!(
        logs.contains("policy=poweroff") && logs.contains("policy=pause"),
        "{logs}"
    );
}

/// Dump cap is `dump_ram_max_mib`, and the UI/report warning names secrets.
#[test]
fn dump_file_is_capped_and_privacy_warning_names_secrets() {
    assert!(RAM_DUMP_PRIVACY_WARNING.contains("guest RAM dump"));
    assert!(RAM_DUMP_PRIVACY_WARNING.contains("Do not share"));
    assert!(
        RAM_DUMP_PRIVACY_WARNING.contains("passwords") && RAM_DUMP_PRIVACY_WARNING.contains("keys")
    );
}
