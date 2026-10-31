//! Relaxed atomic counters. Hot-path methods do not log or allocate.

use std::sync::atomic::{AtomicU64, Ordering};

/// A single `AtomicU64` counter.
#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    /// Add `n`.
    #[inline]
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }

    /// Add one.
    #[inline]
    pub fn inc(&self) {
        self.add(1);
    }

    /// Current value.
    #[inline]
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

#[cfg(test)]
mod tests {
    use super::Counter;
    use std::sync::Arc;
    use std::thread;

    #[test]
    fn eight_threads_sum_without_loss() {
        let counter = Arc::new(Counter::default());
        let per_thread = 10_000u64;
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let counter = Arc::clone(&counter);
                thread::spawn(move || {
                    for _ in 0..per_thread {
                        counter.inc();
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().expect("join");
        }
        assert_eq!(counter.get(), 8 * per_thread);
    }
}
