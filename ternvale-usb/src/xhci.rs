//! xHCI host controller MMIO device.

use std::sync::{Arc, Mutex};

use crate::guest_mem::{GuestMemoryAccess, MemError};
use crate::passthrough::PassthroughWorker;

use crate::command::CommandRing;
use crate::event::EventRing;
use crate::hid::HidKind;
use crate::regs::*;
use crate::slots::SlotTable;
use crate::transfer;

/// Building the controller failed.
#[derive(Debug, thiserror::Error)]
pub enum XhciError {
    /// MMIO base is not page aligned.
    #[error("xhci base {base:#x} is not 4 KiB aligned")]
    Misaligned {
        /// Rejected base.
        base: u64,
    },
}

/// xHCI 1.x model with virtual keyboard and mouse.
pub struct Xhci {
    base: u64,
    memory: Arc<Mutex<dyn GuestMemoryAccess + Send>>,
    usbcmd: u32,
    usbsts: u32,
    config: u32,
    dcbaap: u64,
    iman: u32,
    crcr: u64,
    cmd: CommandRing,
    events: EventRing,
    slots: SlotTable,
    erst_size: u16,
    erst_base: u64,
    erdp: u64,
    irq: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    passthrough: Option<Arc<PassthroughWorker>>,
}

impl Xhci {
    /// Controller at guest physical `base` using `memory` for rings.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all, fields(base = format!("{base:#x}")))]
    pub fn new(
        base: u64,
        memory: Arc<Mutex<dyn GuestMemoryAccess + Send>>,
    ) -> Result<Self, XhciError> {
        Self::new_with_irq(base, memory, None)
    }

    /// Like [`Self::new`] with an optional GIC SPI hook (assert on event post).
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all, fields(base = format!("{base:#x}")))]
    pub fn new_with_irq(
        base: u64,
        memory: Arc<Mutex<dyn GuestMemoryAccess + Send>>,
        irq: Option<Arc<dyn Fn(bool) + Send + Sync>>,
    ) -> Result<Self, XhciError> {
        if base % 0x1000 != 0 {
            tracing::warn!(target: "ternvale::usb", base = format!("{base:#x}"), "xhci base misaligned");
            return Err(XhciError::Misaligned { base });
        }
        tracing::info!(
            target: "ternvale::usb",
            base = format!("{base:#x}"),
            size = format!("{MMIO_SIZE:#x}"),
            ports = NUM_PORTS,
            max_slots = MAX_SLOTS,
            "xhci device ready"
        );
        Ok(Self {
            base,
            memory,
            usbcmd: 0,
            usbsts: STS_HALT,
            config: 0,
            dcbaap: 0,
            iman: 0,
            crcr: 0,
            cmd: CommandRing::default(),
            events: EventRing::default(),
            slots: SlotTable::new(MAX_SLOTS),
            erst_size: 0,
            erst_base: 0,
            erdp: 0,
            irq,
            passthrough: None,
        })
    }

    /// Attach the passthrough worker used to forward transfers to host USB devices.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
    pub fn set_passthrough_worker(&mut self, worker: Arc<PassthroughWorker>) {
        self.passthrough = Some(worker);
        tracing::info!(target: "ternvale::usb", "xhci passthrough worker attached");
    }

    /// Configure root ports and slot table for host passthrough entries `(port, vid, pid)`.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip(self, entries))]
    pub fn configure_passthrough_ports(&mut self, entries: &[(u8, u16, u16)]) {
        self.slots.configure_passthrough_ports(entries);
    }

    /// Post a transfer completion from the passthrough worker thread.
    pub(crate) fn post_passthrough_completion(
        &mut self,
        slot_id: u8,
        dci: u8,
        code: u32,
        trb_ptr: u64,
        bytes: u32,
        mem: &mut dyn GuestMemoryAccess,
    ) -> Result<(), MemError> {
        self.events.post(mem, slot_id, dci, code, trb_ptr, bytes)?;
        Ok(())
    }

    /// Slot table (passthrough worker completion path).
    pub(crate) fn slots_mut(&mut self) -> &mut SlotTable {
        &mut self.slots
    }

    /// Host unplug: drop the guest port/slot and pulse IRQ so the guest observes disconnect.
    #[tracing::instrument(
        level = "warn",
        target = "ternvale::usb",
        skip(self, _mem),
        fields(port)
    )]
    pub fn handle_host_usb_disconnect(&mut self, port: u8, _mem: &mut dyn GuestMemoryAccess) {
        if self.slots.disconnect_passthrough_port(port).is_some() {
            self.pulse_irq();
        }
    }

    pub(crate) fn pulse_irq(&mut self) {
        self.usbsts |= STS_EINT;
        self.iman |= IMAN_IP;
        if let Some(hook) = &self.irq {
            hook(true);
        }
    }

    /// Guest physical base of the MMIO window.
    pub fn base(&self) -> u64 {
        self.base
    }

    /// Point a slot's EP0 transfer ring at `dequeue` (unit tests only).
    #[cfg(test)]
    pub fn seed_ep0_ring(&mut self, slot_id: u8, dequeue: u64) {
        if let Some(slot) = self.slots.slot_mut(slot_id) {
            slot.ep0.dequeue = dequeue;
            slot.ep0.cycle = true;
            slot.ep0.enabled = true;
            tracing::debug!(target: "ternvale::usb", slot_id, dequeue = format!("{dequeue:#x}"), "test ep0 ring seeded");
        }
    }

    /// Queue a HID interrupt IN report for the given virtual device.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip(self, data))]
    pub fn queue_hid_report(&mut self, kind: HidKind, data: Vec<u8>) -> Result<(), MemError> {
        let port = kind.port();
        let Some(slot_id) = self.slots.slot_for_port(port) else {
            tracing::warn!(target: "ternvale::usb", ?kind, "hid report before slot enabled");
            return Ok(());
        };
        if let Some(slot) = self.slots.slot_mut(slot_id) {
            let bytes = data.len();
            slot.pending_in = data;
            tracing::debug!(target: "ternvale::usb", slot_id, bytes, "hid in report pending");
        }
        Ok(())
    }

    /// MMIO read.
    #[tracing::instrument(level = "trace", target = "ternvale::usb", skip(self), fields(offset = format!("{offset:#x}"), size))]
    pub fn read(&self, offset: u64, size: u8) -> u64 {
        let val = self.read_inner(offset);
        let out = match size {
            1 => val as u8 as u64,
            2 => (val as u16) as u64,
            4 => val as u64,
            8 => val as u64,
            _ => {
                tracing::warn!(target: "ternvale::usb", offset = format!("{offset:#x}"), size, "xhci read bad size");
                0
            }
        };
        tracing::trace!(target: "ternvale::usb", offset = format!("{offset:#x}"), size, value = format!("{out:#x}"), "xhci mmio read");
        out
    }

    fn read_inner(&self, offset: u64) -> u32 {
        match offset {
            0 => u32::from(CAPLENGTH),
            0x02 => HCIVERSION as u32,
            HCSPARAMS1 => {
                u32::from(MAX_SLOTS) | (u32::from(MAX_INTRS) << 8) | (u32::from(NUM_PORTS) << 24)
            }
            HCSPARAMS2 => 0,
            HCCPARAMS1 => HCCPARAMS1_VALUE,
            o if (EXT_CAP_BASE..EXT_CAP_BASE + 12).contains(&o) => ext_cap_word(o),
            0x14 => DBOFF,
            0x18 => RTSOFF,
            USBCMD => self.usbcmd,
            USBSTS => self.usbsts,
            PAGESIZE => 0x0000_1000,
            DNCTRL => 0,
            CRCR => self.crcr as u32,
            CRCR_HI => (self.crcr >> 32) as u32,
            CONFIG => self.config,
            DCBAAP => self.dcbaap as u32,
            DCBAAP_HI => (self.dcbaap >> 32) as u32,
            o if o == rt_iman(0) => self.iman,
            o if o >= PORTSC_BASE && o < PORTSC_BASE + u64::from(NUM_PORTS) * 16 => {
                let port = ((o - PORTSC_BASE) / 16) as u8 + 1;
                self.slots.portsc(port)
            }
            o if o == rt_erstsz(0) => self.erst_size as u32,
            o if o == rt_erstba(0) => self.erst_base as u32,
            o if o == rt_erstba(0) + 4 => (self.erst_base >> 32) as u32,
            o if o == rt_erdp(0) => self.erdp as u32,
            o if o == rt_erdp(0) + 4 => (self.erdp >> 32) as u32,
            _ => 0,
        }
    }

    /// MMIO write.
    #[tracing::instrument(level = "trace", target = "ternvale::usb", skip(self), fields(offset = format!("{offset:#x}"), size, value = format!("{val:#x}")))]
    pub fn write(&mut self, offset: u64, size: u8, val: u64) {
        if !matches!(size, 1 | 2 | 4 | 8) {
            tracing::warn!(target: "ternvale::usb", offset = format!("{offset:#x}"), size, "xhci write bad size");
            return;
        }
        if size == 8 {
            match offset {
                CRCR => {
                    self.crcr = val & !0x3f;
                    self.cmd.dequeue = self.crcr;
                    self.cmd.cycle = (val & 1) != 0;
                    tracing::debug!(target: "ternvale::usb", dequeue = format!("{:#x}", self.cmd.dequeue), "xhci crcr updated (64-bit)");
                    return;
                }
                DCBAAP => {
                    self.dcbaap = val;
                    tracing::debug!(target: "ternvale::usb", dcbaap = format!("{:#x}", self.dcbaap), "xhci dcbaap updated (64-bit)");
                    return;
                }
                o if o == rt_erstba(0) => {
                    self.erst_base = val;
                    self.events.erst_base = self.erst_base;
                    return;
                }
                o if o == rt_erdp(0) => {
                    self.erdp = val & !0xf;
                    self.events.index = (((self.erdp & !0xf) - self.erst_base) / 16) as u32;
                    return;
                }
                _ => {}
            }
        }
        let word = val as u32;
        match offset {
            USBCMD => self.write_usbcmd(word),
            CRCR => {
                self.crcr = (self.crcr & 0xffff_ffff_0000_0000) | u64::from(word & !0x3f);
                self.cmd.dequeue = self.crcr & !0x3f;
                self.cmd.cycle = (word & 1) != 0;
                tracing::debug!(target: "ternvale::usb", dequeue = format!("{:#x}", self.cmd.dequeue), "xhci crcr updated");
            }
            CRCR_HI => {
                self.crcr = (self.crcr & 0xffff_ffff) | (u64::from(word) << 32);
            }
            CONFIG => {
                self.config = word & 0xff;
                tracing::debug!(target: "ternvale::usb", max_slots = self.config, "xhci config");
            }
            DCBAAP => {
                self.dcbaap = (self.dcbaap & 0xffff_ffff_0000_0000) | u64::from(word);
                tracing::debug!(target: "ternvale::usb", dcbaap = format!("{:#x}", self.dcbaap), "xhci dcbaap");
            }
            DCBAAP_HI => {
                self.dcbaap = (self.dcbaap & 0xffff_ffff) | (u64::from(word) << 32);
                tracing::debug!(target: "ternvale::usb", dcbaap = format!("{:#x}", self.dcbaap), "xhci dcbaap");
            }
            o if o == rt_iman(0) => {
                if word & IMAN_IP != 0 {
                    self.iman &= !IMAN_IP;
                    self.usbsts &= !STS_EINT;
                    if let Some(hook) = &self.irq {
                        hook(false);
                    }
                }
                self.iman = (self.iman & !IMAN_IE) | (word & IMAN_IE) | (self.iman & IMAN_IP);
            }
            o if o == rt_erstsz(0) => {
                self.erst_size = word as u16;
                self.events.erst_size = self.erst_size;
            }
            o if o == rt_erstba(0) => {
                self.erst_base = (self.erst_base & 0xffff_ffff_0000_0000) | u64::from(word);
                self.events.erst_base = self.erst_base;
            }
            o if o == rt_erstba(0) + 4 => {
                self.erst_base = (self.erst_base & 0xffff_ffff) | (u64::from(word) << 32);
                self.events.erst_base = self.erst_base;
            }
            o if o == rt_erdp(0) => {
                self.erdp = (self.erdp & 0xffff_ffff_0000_0000) | u64::from(word & !0xf);
                self.events.index = (((self.erdp & !0xf) - self.erst_base) / 16) as u32;
            }
            o if o >= PORTSC_BASE && o < PORTSC_BASE + u64::from(NUM_PORTS) * 16 => {
                let port = ((o - PORTSC_BASE) / 16) as u8 + 1;
                self.slots.write_portsc(port, word);
            }
            o if o == USBSTS => {
                self.usbsts &= !(word & (STS_EINT | STS_HSE));
            }
            o if o >= doorbell(0) && o < doorbell(MAX_SLOTS + 1) => {
                let slot = ((o - doorbell(0)) / 4) as u8;
                let target = (word & 0xff) as u8;
                self.ring_doorbell(slot, target);
            }
            _ => {
                tracing::trace!(target: "ternvale::usb", offset = format!("{offset:#x}"), "xhci write ignored")
            }
        }
    }

    fn write_usbcmd(&mut self, val: u32) {
        if val & CMD_HCRST != 0 {
            self.usbcmd = 0;
            self.usbsts = STS_HALT;
            self.crcr = 0;
            self.cmd = CommandRing::default();
            tracing::info!(target: "ternvale::usb", "xhci host controller reset");
            return;
        }
        let run = val & CMD_RUN != 0;
        self.usbcmd = val & (CMD_RUN | CMD_HCRST | CMD_INTE);
        self.cmd.running = run;
        if run {
            self.usbsts &= !STS_HALT;
            tracing::info!(target: "ternvale::usb", "xhci run");
        } else {
            self.usbsts = STS_HALT;
            tracing::info!(target: "ternvale::usb", "xhci stop");
        }
    }

    fn ring_doorbell(&mut self, slot: u8, target: u8) {
        tracing::debug!(target: "ternvale::usb", slot, target, "xhci doorbell");
        let Ok(mut mem) = self.memory.lock() else {
            tracing::error!(target: "ternvale::usb", "guest memory lock poisoned");
            return;
        };
        let posted = if slot == 0 {
            self.cmd
                .run(&mut *mem, &mut self.slots, &mut self.events)
                .is_ok()
        } else if let Some(s) = self.slots.slot_mut(slot) {
            transfer::process_transfer(
                &mut *mem,
                slot,
                target,
                s,
                &mut self.events,
                self.passthrough.as_ref(),
            )
            .is_ok()
        } else {
            false
        };
        drop(mem);
        if posted {
            self.pulse_irq();
        } else if slot == 0 {
            tracing::warn!(target: "ternvale::usb", "command ring failed");
        } else {
            tracing::warn!(target: "ternvale::usb", slot, "transfer ring failed");
        }
    }
}

/// Build a controller and [`UsbInput`] sharing one `Arc<Mutex<Xhci>>`.
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
pub fn attach_shared(
    base: u64,
    memory: Arc<Mutex<dyn GuestMemoryAccess + Send>>,
    irq: Arc<dyn Fn(bool) + Send + Sync>,
) -> Result<(Arc<Mutex<Xhci>>, crate::UsbInput), XhciError> {
    attach_shared_with_passthrough(base, memory, irq, &[], vec![])
}

/// Like [`attach_shared`] with passthrough ports and host backends.
#[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all, fields(passthrough_ports = port_entries.len(), backends = bindings.len()))]
pub fn attach_shared_with_passthrough(
    base: u64,
    memory: Arc<Mutex<dyn GuestMemoryAccess + Send>>,
    irq: Arc<dyn Fn(bool) + Send + Sync>,
    port_entries: &[(u8, u16, u16)],
    bindings: Vec<crate::passthrough::PassthroughBinding>,
) -> Result<(Arc<Mutex<Xhci>>, crate::UsbInput), XhciError> {
    let mut xhci = Xhci::new_with_irq(base, Arc::clone(&memory), Some(irq))?;
    xhci.configure_passthrough_ports(port_entries);
    let inner = Arc::new(Mutex::new(xhci));
    if !bindings.is_empty() {
        let worker = Arc::new(PassthroughWorker::start(
            bindings,
            Arc::clone(&memory),
            Arc::clone(&inner),
        ));
        if let Ok(mut guard) = inner.lock() {
            guard.set_passthrough_worker(worker);
        } else {
            tracing::error!(target: "ternvale::usb", "xhci lock poisoned installing passthrough worker");
        }
    }
    let input = crate::UsbInput::new(Arc::clone(&inner));
    Ok((inner, input))
}
