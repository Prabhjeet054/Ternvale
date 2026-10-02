//! Bus 0: function slots, config routing, BAR routing, host BAR assignment.

use std::sync::Arc;

use super::config::{STATUS, STATUS_INTERRUPT};
use super::{Bdf, IntxPin, PciFunction};

/// Device numbers on one bus.
pub(super) const DEVICES: usize = 32;

struct Slot {
    function: Box<dyn PciFunction>,
    pin: Option<Arc<IntxPin>>,
}

/// Decode-relevant state, compared around each config write for logging.
#[derive(PartialEq, Eq)]
struct Decode {
    command: u16,
    bars: Vec<(usize, u64, u64)>,
}

/// Functions on bus 0 and the MMIO window their BARs must sit in.
pub(super) struct PciBus {
    slots: Vec<Option<Slot>>,
    window_base: u64,
    window_size: u64,
}

impl PciBus {
    pub(super) fn new(window_base: u64, window_size: u64) -> Self {
        Self {
            slots: (0..DEVICES).map(|_| None).collect(),
            window_base,
            window_size,
        }
    }

    /// Lowest free device number.
    pub(super) fn free_device(&self) -> Option<u8> {
        self.slots.iter().position(Option::is_none).map(|d| d as u8)
    }

    pub(super) fn insert(
        &mut self,
        device: u8,
        function: Box<dyn PciFunction>,
        pin: Option<Arc<IntxPin>>,
    ) {
        if let Some(slot) = self.slots.get_mut(usize::from(device)) {
            *slot = Some(Slot { function, pin });
        }
    }

    fn slot(&mut self, bdf: Bdf) -> Option<&mut Slot> {
        if bdf.bus != 0 || bdf.function != 0 {
            return None;
        }
        self.slots.get_mut(usize::from(bdf.device))?.as_mut()
    }

    /// Config read; `None` for an absent function (the guest sees all-ones).
    pub(super) fn config_read(&mut self, bdf: Bdf, reg: u16, size: u8) -> Option<u32> {
        let slot = self.slot(bdf)?;
        let mut value = slot.function.read_config(reg, size);
        let status = STATUS as u16;
        if reg <= status
            && status < reg + u16::from(size)
            && slot.pin.as_ref().is_some_and(|p| p.asserted())
        {
            value |= u32::from(STATUS_INTERRUPT) << (8 * (status - reg));
        }
        Some(value)
    }

    /// Config write. Logs command and BAR changes; mirrors INTx disable.
    /// Returns what the register holds afterwards (after the write masks), or
    /// `None` for an absent function.
    pub(super) fn config_write(&mut self, bdf: Bdf, reg: u16, size: u8, value: u32) -> Option<u32> {
        let (window_base, window_end) = (self.window_base, self.window_base + self.window_size);
        let Some(slot) = self.slot(bdf) else {
            tracing::debug!(
                target: "ternvale::pci",
                %bdf,
                reg = format!("{reg:#x}"),
                value = format!("{value:#x}"),
                "config write to an absent function ignored"
            );
            return None;
        };
        let before = decode(slot.function.as_ref());
        slot.function.write_config(reg, size, value);
        let after = decode(slot.function.as_ref());
        if let Some(pin) = &slot.pin {
            pin.set_disabled(slot.function.config().intx_disabled());
        }
        if before.command != after.command {
            tracing::debug!(
                target: "ternvale::pci",
                %bdf,
                function = slot.function.name(),
                from = format!("{:#06x}", before.command),
                to = format!("{:#06x}", after.command),
                memory = slot.function.config().memory_enabled(),
                "pci command"
            );
        }
        if before.bars != after.bars {
            for &(bar, base, size) in &after.bars {
                let inside = base >= window_base && base.saturating_add(size) <= window_end;
                tracing::info!(
                    target: "ternvale::pci",
                    %bdf,
                    function = slot.function.name(),
                    bar,
                    base = format!("{base:#x}"),
                    size = format!("{size:#x}"),
                    inside_window = inside,
                    "pci bar decoding"
                );
            }
        }
        Some(slot.function.config().read(reg, size))
    }

    /// Route a window access to the BAR that decodes `gpa`.
    pub(super) fn mmio_read(&mut self, gpa: u64, size: u8) -> Option<u64> {
        let (device, bar, offset) = self.route(gpa, size)?;
        let slot = self.slots[device].as_mut()?;
        Some(slot.function.read_bar(bar, offset, size))
    }

    pub(super) fn mmio_write(&mut self, gpa: u64, size: u8, value: u64) -> bool {
        let Some((device, bar, offset)) = self.route(gpa, size) else {
            return false;
        };
        let Some(slot) = self.slots[device].as_mut() else {
            return false;
        };
        slot.function.write_bar(bar, offset, size, value);
        true
    }

    fn route(&self, gpa: u64, size: u8) -> Option<(usize, usize, u64)> {
        let end = gpa.checked_add(u64::from(size))?;
        self.slots.iter().enumerate().find_map(|(device, slot)| {
            let config = slot.as_ref()?.function.config();
            config
                .decoded_bars()
                .into_iter()
                .find(|&(_, base, len)| gpa >= base && end <= base.saturating_add(len))
                .map(|(bar, base, _)| (device, bar, gpa - base))
        })
    }

    /// Firmware-style assignment: largest BAR first, naturally aligned, from
    /// the bottom of the window. Decode stays off; the guest enables it (Linux
    /// reassigns anyway because nothing claims these resources).
    pub(super) fn assign_bars(&mut self) -> usize {
        let mut wanted = Vec::new();
        for (device, slot) in self.slots.iter().enumerate() {
            let Some(slot) = slot else { continue };
            for index in 0..6 {
                if let Some(bar) = slot.function.config().bar(index) {
                    wanted.push((bar.size, device, index));
                }
            }
        }
        wanted.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)).then(a.2.cmp(&b.2)));
        let end = self.window_base + self.window_size;
        let mut next = self.window_base;
        let mut assigned = 0;
        for (size, device, index) in wanted {
            let base = next.next_multiple_of(size);
            let Some(slot) = self.slots[device].as_mut() else {
                continue;
            };
            let bdf = Bdf::new(device as u8);
            if base.saturating_add(size) > end {
                tracing::warn!(
                    target: "ternvale::pci",
                    %bdf,
                    bar = index,
                    size = format!("{size:#x}"),
                    "pci window exhausted; bar left unassigned"
                );
                continue;
            }
            slot.function.config_mut().set_bar_address(index, base);
            next = base + size;
            assigned += 1;
            tracing::info!(
                target: "ternvale::pci",
                %bdf,
                function = slot.function.name(),
                bar = index,
                base = format!("{base:#x}"),
                size = format!("{size:#x}"),
                "pci bar assigned"
            );
        }
        assigned
    }

    /// `(bdf, name)` of every populated function, for the startup log.
    pub(super) fn functions(&self) -> Vec<(Bdf, String)> {
        self.slots
            .iter()
            .enumerate()
            .filter_map(|(device, slot)| {
                let slot = slot.as_ref()?;
                Some((Bdf::new(device as u8), slot.function.name().to_string()))
            })
            .collect()
    }
}

fn decode(function: &dyn PciFunction) -> Decode {
    let config = function.config();
    Decode {
        command: config.command(),
        bars: config.decoded_bars(),
    }
}

pub(super) fn all_ones(size: u8) -> u32 {
    match size {
        1 => 0xff,
        2 => 0xffff,
        _ => 0xffff_ffff,
    }
}
