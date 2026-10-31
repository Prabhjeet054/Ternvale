//! Fixed log-scale latency histogram. Each record is atomic increments only.

use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

/// One bucket per power of two nanoseconds: `[0]`, then `[2^i, 2^{i+1})`.
pub const BUCKETS: usize = 64;

/// Lock-free log₂ latency histogram in nanoseconds.
#[derive(Debug)]
pub struct Histogram {
    counts: [AtomicU64; BUCKETS],
    sum_ns: AtomicU64,
    samples: AtomicU64,
    max_ns: AtomicU64,
}

impl Default for Histogram {
    fn default() -> Self {
        Self {
            counts: std::array::from_fn(|_| AtomicU64::new(0)),
            sum_ns: AtomicU64::new(0),
            samples: AtomicU64::new(0),
            max_ns: AtomicU64::new(0),
        }
    }
}

impl Histogram {
    /// Record one sample of `ns` nanoseconds. No locks, no allocation.
    #[inline]
    pub fn record(&self, ns: u64) {
        let i = bucket(ns);
        self.counts[i].fetch_add(1, Ordering::Relaxed);
        self.sum_ns.fetch_add(ns, Ordering::Relaxed);
        self.samples.fetch_add(1, Ordering::Relaxed);
        self.max_ns.fetch_max(ns, Ordering::Relaxed);
    }

    /// Copy counters into a serializable snapshot.
    #[tracing::instrument(level = "debug", target = "ternvale::metrics", skip_all)]
    pub fn snapshot(&self) -> HistSnapshot {
        let mut counts = [0u64; BUCKETS];
        for (i, slot) in self.counts.iter().enumerate() {
            counts[i] = slot.load(Ordering::Relaxed);
        }
        let samples = self.samples.load(Ordering::Relaxed);
        HistSnapshot {
            samples,
            sum_ns: self.sum_ns.load(Ordering::Relaxed),
            max_ns: self.max_ns.load(Ordering::Relaxed),
            p50_ns: percentile(&counts, samples, 50),
            p99_ns: percentile(&counts, samples, 99),
        }
    }
}

/// Percentiles are the inclusive upper edge of the bucket that contains them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistSnapshot {
    /// Samples recorded.
    pub samples: u64,
    /// Sum of sample nanoseconds.
    pub sum_ns: u64,
    /// Exact maximum sample.
    pub max_ns: u64,
    /// Approximate p50 (bucket high bound), or 0 when empty.
    pub p50_ns: u64,
    /// Approximate p99 (bucket high bound), or 0 when empty.
    pub p99_ns: u64,
}

#[inline]
fn bucket(ns: u64) -> usize {
    if ns <= 1 {
        0
    } else {
        (63 - ns.leading_zeros() as usize).min(BUCKETS - 1)
    }
}

fn bucket_high(index: usize) -> u64 {
    if index == 0 {
        1
    } else if index >= 63 {
        u64::MAX
    } else {
        (1u64 << (index + 1)) - 1
    }
}

fn percentile(counts: &[u64; BUCKETS], samples: u64, pct: u64) -> u64 {
    if samples == 0 {
        return 0;
    }
    let want = samples.saturating_mul(pct).div_ceil(100).max(1);
    let mut seen = 0u64;
    for (i, count) in counts.iter().enumerate() {
        seen = seen.saturating_add(*count);
        if seen >= want {
            return bucket_high(i);
        }
    }
    bucket_high(BUCKETS - 1)
}

#[cfg(test)]
mod tests {
    use super::{bucket, Histogram};

    #[test]
    fn identical_samples_share_one_bucket_and_exact_max() {
        let hist = Histogram::default();
        for _ in 0..1_000 {
            hist.record(100);
        }
        let snap = hist.snapshot();
        assert_eq!(snap.samples, 1_000);
        assert_eq!(snap.sum_ns, 100_000);
        assert_eq!(snap.max_ns, 100);
        let high = super::bucket_high(bucket(100));
        assert_eq!(snap.p50_ns, high);
        assert_eq!(snap.p99_ns, high);
        assert!(high >= 100, "bucket high {high} covers 100ns");
    }

    #[test]
    fn known_two_point_distribution_p50_p99_max() {
        let hist = Histogram::default();
        for _ in 0..50 {
            hist.record(4);
        }
        for _ in 0..50 {
            hist.record(64);
        }
        let snap = hist.snapshot();
        assert_eq!(snap.samples, 100);
        assert_eq!(snap.max_ns, 64);
        assert_eq!(snap.p50_ns, super::bucket_high(bucket(4)));
        assert_eq!(snap.p99_ns, super::bucket_high(bucket(64)));
        assert!(snap.p99_ns > snap.p50_ns);
    }

    #[test]
    fn spread_values_raise_p99_above_p50() {
        let hist = Histogram::default();
        for _ in 0..1_000 {
            hist.record(8);
        }
        for _ in 0..20 {
            hist.record(1 << 20);
        }
        let snap = hist.snapshot();
        assert_eq!(snap.max_ns, 1 << 20);
        assert!(
            snap.p99_ns > snap.p50_ns,
            "p50={} p99={}",
            snap.p50_ns,
            snap.p99_ns
        );
        assert!(snap.p50_ns >= 8);
        assert!(snap.p99_ns >= 1 << 20);
    }
}
