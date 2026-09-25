//! Hang detector: warn when the guest produces no MMIO or exit for too long.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

const EVENT_LIMIT: usize = 20;

/// Last exits the hang log prints.
pub struct Watchdog {
    interval: Duration,
    last: Mutex<Instant>,
    pc: Mutex<u64>,
    events: Mutex<VecDeque<String>>,
    reported: Mutex<bool>,
}

impl Watchdog {
    /// Warn after `interval` with no [`Watchdog::note_exit`].
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all)]
    pub fn new(interval: Duration) -> Self {
        tracing::info!(
            target: "ternvale::vcpu",
            secs = interval.as_secs(),
            "hang watchdog armed"
        );
        Self {
            interval,
            last: Mutex::new(Instant::now()),
            pc: Mutex::new(0),
            events: Mutex::new(VecDeque::new()),
            reported: Mutex::new(false),
        }
    }

    /// Record an exit and its PC. Clears a previous hang so the next stall warns again.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(pc = format!("{:#x}", pc)))]
    pub fn note_exit(&self, pc: u64) {
        *self.lock(&self.last) = Instant::now();
        *self.lock(&self.pc) = pc;
        *self.lock(&self.reported) = false;
    }

    /// Remember one MMIO access. Only the last 20 are kept.
    #[tracing::instrument(level = "debug", target = "ternvale::mmio", skip_all)]
    pub fn note_mmio(&self, line: String) {
        let mut events = self.lock(&self.events);
        if events.len() == EVENT_LIMIT {
            events.pop_front();
        }
        events.push_back(line);
    }

    /// `Some` once when the guest has been idle for the interval.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all)]
    pub fn poll(&self) -> Option<String> {
        let idle = self.lock(&self.last).elapsed();
        if idle < self.interval {
            return None;
        }
        let mut reported = self.lock(&self.reported);
        if *reported {
            return None;
        }
        *reported = true;
        let pc = *self.lock(&self.pc);
        let events: Vec<String> = self.lock(&self.events).iter().cloned().collect();
        let text = format!(
            "no guest exit for {}s pc={pc:#x} last_mmio={events:?}",
            idle.as_secs()
        );
        tracing::warn!(target: "ternvale::vcpu", pc = format!("{:#x}", pc), events = ?events, "guest hang");
        Some(text)
    }

    fn lock<'a, T>(&'a self, mutex: &'a Mutex<T>) -> std::sync::MutexGuard<'a, T> {
        mutex.lock().unwrap_or_else(|poison| poison.into_inner())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hang_warns_with_the_last_twenty_mmio_events_and_pc() {
        let dog = Watchdog::new(Duration::from_millis(1));
        dog.note_exit(0x4000_1000);
        for index in 0..25 {
            dog.note_mmio(format!("write {index:#x}"));
        }
        *dog.lock(&dog.last) = Instant::now() - Duration::from_secs(11);
        let text = dog.poll().expect("hang");
        assert!(text.contains("pc=0x40001000"), "{text}");
        assert!(text.contains("write 0x18"), "{text}");
        assert!(!text.contains("write 0x0\""), "{text}");
        assert!(dog.poll().is_none(), "a hang is logged once");
        dog.note_exit(0x4000_2000);
        *dog.lock(&dog.last) = Instant::now() - Duration::from_secs(11);
        let again = dog.poll().expect("rearmed");
        assert!(again.contains("pc=0x40002000"), "{again}");
    }
}
