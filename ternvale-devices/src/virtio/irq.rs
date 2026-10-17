//! Shared virtio interrupt-status line and optional GIC hook.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Called with `true` when the line goes active and `false` when it clears.
pub type IrqHook = Arc<dyn Fn(bool) + Send + Sync>;

/// Interrupt status bits shared by the MMIO transport and the device worker.
///
/// `raise` runs on device threads while `ack` runs on the vCPU. Each updates `bits`
/// and then re-syncs the line to the current bits under the `line` lock, so the
/// last sync always sees both updates: the line can never be left low while an
/// interrupt is pending (a lost interrupt stalls the queue for good) or high
/// with nothing pending.
pub struct VirtioIrq {
    bits: AtomicU32,
    line: Mutex<Line>,
}

struct Line {
    hook: Option<IrqHook>,
    /// Level last given to the hook.
    level: bool,
}

impl VirtioIrq {
    /// Idle line with no hook.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::mmio")]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            bits: AtomicU32::new(0),
            line: Mutex::new(Line {
                hook: None,
                level: false,
            }),
        })
    }

    /// Install the GIC (or test) callback.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::mmio", skip(self, hook))]
    pub fn set_hook(&self, hook: IrqHook) {
        ternvale_vmm::lockwatch::lock(&self.line, "virtio-irq-line").hook = Some(hook);
        self.sync_line();
    }

    /// Current interrupt-status register value.
    pub fn status(&self) -> u32 {
        self.bits.load(Ordering::Acquire)
    }

    /// OR `bits` into the status. Raises the line when it becomes non-zero.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::mmio",
        skip(self),
        fields(bits)
    )]
    pub fn raise(&self, bits: u32) {
        let bits = bits & 0b11;
        if bits == 0 {
            return;
        }
        let prev = self.bits.fetch_or(bits, Ordering::AcqRel);
        tracing::debug!(
            target: "ternvale::virtio::mmio",
            interrupt = prev | bits,
            "virtio interrupt raised"
        );
        if prev == 0 {
            self.sync_line();
        }
    }

    /// Clear `bits` from the status. Lowers the line when it becomes zero.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::mmio",
        skip(self),
        fields(bits)
    )]
    pub fn ack(&self, bits: u32) {
        let bits = bits & 0b11;
        let mut prev = self.bits.load(Ordering::Acquire);
        loop {
            let next = prev & !bits;
            match self
                .bits
                .compare_exchange(prev, next, Ordering::AcqRel, Ordering::Acquire)
            {
                Ok(_) => {
                    tracing::debug!(
                        target: "ternvale::virtio::mmio",
                        ack = bits,
                        interrupt = next,
                        "virtio interrupt ack"
                    );
                    if prev != 0 && next == 0 {
                        self.sync_line();
                    }
                    return;
                }
                Err(current) => prev = current,
            }
        }
    }

    /// Drive the line to match the status bits as they are now.
    fn sync_line(&self) {
        let mut line = ternvale_vmm::lockwatch::lock(&self.line, "virtio-irq-line");
        let level = self.bits.load(Ordering::Acquire) != 0;
        if line.level == level {
            return;
        }
        let Some(hook) = line.hook.clone() else {
            return;
        };
        line.level = level;
        tracing::trace!(target: "ternvale::virtio::mmio", level, "virtio interrupt line");
        hook(level);
    }
}

#[cfg(test)]
#[path = "irq_tests.rs"]
mod tests;
