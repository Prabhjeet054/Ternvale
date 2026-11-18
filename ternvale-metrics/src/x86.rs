//! Per-engine x86 translation counters (guest usermode, not the vCPU loop).

use std::sync::atomic::{AtomicU64, Ordering};

use crate::snapshot::X86EngineSnapshot;

/// Engines that `query-stats.metrics.x86` may name (stable strings).
pub const X86_ENGINES: [&str; 3] = ["rosetta", "fex", "qemu-user"];

/// Translated-process and failure counts for Rosetta / FEX / qemu-user.
#[derive(Debug)]
pub struct X86Counters {
    processes: [AtomicU64; 3],
    failures: [AtomicU64; 3],
}

impl Default for X86Counters {
    fn default() -> Self {
        Self {
            processes: std::array::from_fn(|_| AtomicU64::new(0)),
            failures: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }
}

impl X86Counters {
    /// Slot for a status `x86_engine` name, or `None` if unknown.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::metrics",
        skip_all,
        fields(engine)
    )]
    pub fn index(engine: &str) -> Option<usize> {
        X86_ENGINES.iter().position(|name| *name == engine)
    }

    /// Add translated-process and failure deltas. Unknown engines log WARN.
    #[inline]
    pub fn add(&self, engine: &str, processes: u64, failures: u64) -> bool {
        let Some(index) = Self::index(engine) else {
            tracing::warn!(
                target: "ternvale::metrics",
                engine,
                "unknown x86 engine for metrics"
            );
            return false;
        };
        if processes != 0 {
            self.processes[index].fetch_add(processes, Ordering::Relaxed);
        }
        if failures != 0 {
            self.failures[index].fetch_add(failures, Ordering::Relaxed);
        }
        tracing::debug!(
            target: "ternvale::metrics",
            engine,
            processes,
            failures,
            "x86 engine counters updated"
        );
        true
    }

    /// Non-zero rows for the snapshot (allocates only here).
    #[tracing::instrument(level = "debug", target = "ternvale::metrics", skip_all)]
    pub fn snapshot(&self) -> Vec<X86EngineSnapshot> {
        X86_ENGINES
            .iter()
            .enumerate()
            .filter_map(|(index, name)| {
                let processes = self.processes[index].load(Ordering::Relaxed);
                let failures = self.failures[index].load(Ordering::Relaxed);
                if processes == 0 && failures == 0 {
                    None
                } else {
                    Some(X86EngineSnapshot {
                        engine: (*name).to_string(),
                        processes,
                        failures,
                    })
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::X86Counters;

    #[test]
    fn unknown_engine_is_ignored() {
        let counters = X86Counters::default();
        assert!(!counters.add("box64", 1, 0));
        assert!(counters.snapshot().is_empty());
    }

    #[test]
    fn qemu_user_counts_processes_and_failures() {
        let counters = X86Counters::default();
        assert!(counters.add("qemu-user", 2, 1));
        assert!(counters.add("fex", 4, 0));
        let snap = counters.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].engine, "fex");
        assert_eq!(snap[0].processes, 4);
        assert_eq!(snap[1].engine, "qemu-user");
        assert_eq!(snap[1].failures, 1);
        assert_eq!(snap[1].processes, 2);
    }
}
