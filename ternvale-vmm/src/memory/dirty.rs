//! Per-region dirty-page bits. Guest stores that fault at stage 2 and every
//! host-side [`super::GuestMemory`] write helper set these bits.

use std::sync::atomic::{AtomicU64, Ordering};

use super::HOST_PAGE_SIZE;

/// One bit per host page covering a guest region.
pub(crate) struct DirtyBits {
    words: Box<[AtomicU64]>,
}

impl DirtyBits {
    pub(super) fn new(size: usize) -> Self {
        let pages = size / HOST_PAGE_SIZE as usize;
        let n = pages.div_ceil(64).max(1);
        Self {
            words: (0..n)
                .map(|_| AtomicU64::new(0))
                .collect::<Vec<_>>()
                .into_boxed_slice(),
        }
    }

    /// Mark every page overlapping `offset..offset+len` inside the region.
    pub(super) fn mark_offset(&self, offset: usize, len: usize) {
        if len == 0 {
            return;
        }
        let page = HOST_PAGE_SIZE as usize;
        let start = offset / page;
        let end = offset.saturating_add(len - 1) / page;
        for index in start..=end {
            let word = index / 64;
            let bit = index % 64;
            if let Some(slot) = self.words.get(word) {
                slot.fetch_or(1u64 << bit, Ordering::Release);
            }
        }
    }

    /// Page indices that were dirty, then clear.
    pub(super) fn take(&self) -> Vec<usize> {
        let mut out = Vec::new();
        for (word_i, word) in self.words.iter().enumerate() {
            let mut bits = word.swap(0, Ordering::AcqRel);
            while bits != 0 {
                let bit = bits.trailing_zeros() as usize;
                out.push(word_i * 64 + bit);
                bits &= bits - 1;
            }
        }
        out
    }

    pub(super) fn is_empty(&self) -> bool {
        self.words.iter().all(|w| w.load(Ordering::Acquire) == 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn marks_covering_pages_and_clears_on_take() {
        let bits = DirtyBits::new(HOST_PAGE_SIZE as usize * 3);
        bits.mark_offset(HOST_PAGE_SIZE as usize - 1, 2);
        let pages = bits.take();
        assert_eq!(pages, vec![0, 1]);
        assert!(bits.take().is_empty());
    }
}
