//! Legacy INTx: four level-triggered SPIs shared by every function.
//!
//! Pin `p` (1..=4) of device `d` drives line `(d + p - 1) % 4`, the standard
//! swizzle, so the DT `interrupt-map` only needs devices 0..4 with mask
//! `0x1800`. A line is high while any pin routed to it is asserted and not
//! masked by `COMMAND_INTX_DISABLE`.

use std::sync::{Arc, Mutex};

/// Number of INTx lines (INTA..INTD after swizzling).
pub const PCI_INTX_LINES: usize = 4;
/// FDT SPI number of line 0. QEMU virt uses SPIs 3..=6 for PCIe INTx.
pub const PCI_INTX_SPI0: u32 = 3;

/// Drives one GIC SPI to the given level.
pub type LineHook = Arc<dyn Fn(bool) + Send + Sync>;

/// Line index for `pin` (1..=4) of `device`. Pin 0 (no INTx) maps like INTA.
pub fn swizzle(device: u8, pin: u8) -> usize {
    (usize::from(device) + usize::from(pin.max(1)) - 1) % PCI_INTX_LINES
}

/// One shared level line. Counts asserted pins; the hook runs under the lock
/// so level changes reach the GIC in the order they happened.
pub struct IntxLine {
    spi: u32,
    asserted: Mutex<u32>,
    hook: LineHook,
}

impl IntxLine {
    /// Line for FDT SPI `spi`.
    pub fn new(spi: u32, hook: LineHook) -> Arc<Self> {
        Arc::new(Self {
            spi,
            asserted: Mutex::new(0),
            hook,
        })
    }

    /// FDT SPI number.
    pub fn spi(&self) -> u32 {
        self.spi
    }

    fn adjust(&self, up: bool) {
        let mut count = crate::lockwatch::lock(&self.asserted, "pci-intx-line");
        let before = *count;
        *count = if up {
            before.saturating_add(1)
        } else {
            before.saturating_sub(1)
        };
        if (before == 0) != (*count == 0) {
            tracing::debug!(
                target: "ternvale::pci",
                spi = self.spi,
                level = *count != 0,
                asserted = *count,
                "pci intx line"
            );
            (self.hook)(*count != 0);
        }
    }
}

#[derive(Debug, Default)]
struct PinState {
    requested: bool,
    disabled: bool,
    driving: bool,
}

/// One function's INTx pin.
pub struct IntxPin {
    name: String,
    line: Arc<IntxLine>,
    state: Mutex<PinState>,
}

impl IntxPin {
    /// Pin for function `name` routed to `line`.
    pub fn new(name: String, line: Arc<IntxLine>) -> Arc<Self> {
        Arc::new(Self {
            name,
            line,
            state: Mutex::new(PinState::default()),
        })
    }

    /// Assert or deassert the pin.
    pub fn set(&self, level: bool) {
        self.update(|state| state.requested = level);
    }

    /// Re-read the level inside the pin lock. Callers whose level can change
    /// on several threads use this so the last caller always sees the final
    /// state, whatever order the callbacks run in.
    pub fn refresh(&self, level: &dyn Fn() -> bool) {
        self.update(|state| state.requested = level());
    }

    /// Mirror `COMMAND_INTX_DISABLE`.
    pub fn set_disabled(&self, disabled: bool) {
        self.update(|state| {
            if state.disabled != disabled {
                tracing::debug!(
                    target: "ternvale::pci",
                    function = %self.name,
                    disabled,
                    "pci intx disable"
                );
            }
            state.disabled = disabled;
        });
    }

    /// Requested level, independent of INTx disable. Feeds `STATUS_INTERRUPT`.
    pub fn asserted(&self) -> bool {
        crate::lockwatch::lock(&self.state, "pci-intx-pin").requested
    }

    /// FDT SPI this pin drives.
    pub fn spi(&self) -> u32 {
        self.line.spi()
    }

    fn update(&self, change: impl FnOnce(&mut PinState)) {
        let mut state = crate::lockwatch::lock(&self.state, "pci-intx-pin");
        change(&mut state);
        let want = state.requested && !state.disabled;
        if want != state.driving {
            state.driving = want;
            tracing::trace!(
                target: "ternvale::pci",
                function = %self.name,
                spi = self.line.spi(),
                level = want,
                "pci intx pin"
            );
            self.line.adjust(want);
        }
    }
}
