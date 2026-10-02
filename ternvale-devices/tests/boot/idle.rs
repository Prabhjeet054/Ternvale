//! Host CPU cost of a guest idle period. A watcher thread samples the
//! process CPU clock when each marker line first appears in the serial log.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Marker the guest prints before it idles.
pub const START: &str = "IDLE_A";
/// Marker the guest prints after it idles.
pub const END: &str = "IDLE_B";
/// Highest host CPU use allowed while the guest idles, in percent of one
/// core, for the whole process (every vCPU plus the host threads). A
/// spinning idle loop costs 100% per guest CPU.
pub const MAX_HOST_PCT: u64 = 50;

/// Host CPU used between the two markers.
#[derive(Debug, Clone, Copy)]
pub struct IdleCost {
    pub wall_ms: u64,
    pub cpu_ms: u64,
}

impl IdleCost {
    /// Host CPU as a percentage of one core over the window.
    pub fn host_pct(&self) -> u64 {
        self.cpu_ms * 100 / self.wall_ms.max(1)
    }
}

/// Watches `serial_log` until both markers are seen or `stop` is set.
pub fn watch(serial_log: PathBuf, stop: Arc<AtomicBool>) -> JoinHandle<Option<IdleCost>> {
    std::thread::spawn(move || {
        let mut start: Option<(Instant, u64)> = None;
        while !stop.load(Ordering::Acquire) {
            let text = std::fs::read_to_string(&serial_log).unwrap_or_default();
            let seen = |marker: &str| text.lines().any(|line| line.trim() == marker);
            match start {
                None if seen(START) => {
                    start = Some((Instant::now(), ternvale_vmm::process_cpu_ms()?));
                }
                Some((at, cpu)) if seen(END) => {
                    let cost = IdleCost {
                        wall_ms: at.elapsed().as_millis() as u64,
                        cpu_ms: ternvale_vmm::process_cpu_ms()?.saturating_sub(cpu),
                    };
                    tracing::info!(
                        target: "ternvale::boot",
                        wall_ms = cost.wall_ms,
                        cpu_ms = cost.cpu_ms,
                        host_pct = cost.host_pct(),
                        "host cpu while the guest idled"
                    );
                    return Some(cost);
                }
                _ => {}
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        None
    })
}

/// Fails when the idle window was not seen or cost more than [`MAX_HOST_PCT`].
pub fn check(cost: Option<IdleCost>) -> Result<(), String> {
    let cost = cost.ok_or("idle window not measured (markers missing or cpu clock failed)")?;
    if cost.host_pct() > MAX_HOST_PCT {
        return Err(format!(
            "host used {}% of a core while the guest idled ({} ms cpu in {} ms); limit {MAX_HOST_PCT}%",
            cost.host_pct(),
            cost.cpu_ms,
            cost.wall_ms
        ));
    }
    Ok(())
}
