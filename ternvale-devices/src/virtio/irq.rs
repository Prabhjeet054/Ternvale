//! Shared virtio interrupt-status line and optional GIC hook.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

/// Called with `true` when the line goes active and `false` when it clears.
pub type IrqHook = Arc<dyn Fn(bool) + Send + Sync>;

/// Interrupt status bits shared by the MMIO transport and the device worker.
pub struct VirtioIrq {
    bits: AtomicU32,
    hook: Mutex<Option<IrqHook>>,
}

impl VirtioIrq {
    /// Idle line with no hook.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::mmio")]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            bits: AtomicU32::new(0),
            hook: Mutex::new(None),
        })
    }

    /// Install the GIC (or test) callback.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::mmio", skip(self, hook))]
    pub fn set_hook(&self, hook: IrqHook) {
        match self.hook.lock() {
            Ok(mut slot) => *slot = Some(hook),
            Err(poison) => *poison.into_inner() = Some(hook),
        }
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
            self.call_hook(true);
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
                        self.call_hook(false);
                    }
                    return;
                }
                Err(current) => prev = current,
            }
        }
    }

    fn call_hook(&self, level: bool) {
        let hook = match self.hook.lock() {
            Ok(guard) => guard.clone(),
            Err(poison) => poison.into_inner().clone(),
        };
        if let Some(hook) = hook {
            hook(level);
        }
    }
}
