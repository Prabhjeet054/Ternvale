//! Device slots, root ports, and endpoint transfer rings.

use crate::hid::HidKind;
use crate::regs;

/// Endpoint transfer ring state.
#[derive(Debug, Clone, Default)]
pub struct EpRing {
    /// Guest dequeue pointer (producer).
    pub dequeue: u64,
    /// Cycle bit expected on the ring.
    pub cycle: bool,
    /// Max packet size.
    pub max_packet: u16,
    /// Endpoint is configured and active.
    pub enabled: bool,
}

/// One device slot (xHCI slot id 1..=MAX_SLOTS).
#[derive(Debug, Clone)]
pub struct Slot {
    /// Zero when the slot is free.
    pub port: u8,
    /// USB device address.
    pub address: u8,
    /// Attached HID kind, if any.
    pub hid: Option<HidKind>,
    /// EP0 default control pipe.
    pub ep0: EpRing,
    /// Interrupt IN (EP1).
    pub ep_in: EpRing,
    /// Pending IN report for HID interrupt endpoint.
    pub pending_in: Vec<u8>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            port: 0,
            address: 0,
            hid: None,
            ep0: EpRing::default(),
            ep_in: EpRing {
                max_packet: 8,
                ..EpRing::default()
            },
            pending_in: Vec::new(),
        }
    }
}

impl Slot {
    /// `true` when the slot is allocated.
    pub fn in_use(&self) -> bool {
        self.port != 0
    }
}

/// Root port and slot allocator.
#[derive(Debug)]
pub struct SlotTable {
    ports: [Port; regs::NUM_PORTS as usize],
    slots: Vec<Slot>,
}

/// One root hub port.
#[derive(Debug, Clone, Copy)]
pub struct Port {
    /// Device connected (CCS).
    pub connected: bool,
    /// Port enabled.
    pub enabled: bool,
    /// Which HID is wired to this port.
    pub kind: HidKind,
}

impl SlotTable {
    /// Ports start with keyboard and mouse connected.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
    pub fn new(max_slots: u8) -> Self {
        let ports = [
            Port {
                connected: true,
                enabled: false,
                kind: HidKind::Keyboard,
            },
            Port {
                connected: true,
                enabled: false,
                kind: HidKind::Mouse,
            },
        ];
        tracing::info!(
            target: "ternvale::usb",
            ports = regs::NUM_PORTS,
            max_slots,
            "xhci virtual ports: keyboard on port 1, mouse on port 2"
        );
        Self {
            ports,
            slots: vec![Slot::default(); max_slots as usize],
        }
    }

    /// Port status `PORTSC` value.
    pub fn portsc(&self, port: u8) -> u32 {
        let index = port as usize - 1;
        let Some(p) = self.ports.get(index) else {
            return 0;
        };
        let mut v = 0u32;
        if p.connected {
            v |= regs::PORT_CCS;
            v |= 1 << 17; // CSC — connect status change (xHCI §5.4.8)
        }
        if p.enabled {
            v |= regs::PORT_PED;
        }
        v
    }

    /// Apply a guest `PORTSC` write (reset / enable).
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip(self), fields(port, value = format!("{value:#x}")))]
    pub fn write_portsc(&mut self, port: u8, value: u32) {
        let index = port as usize - 1;
        let Some(p) = self.ports.get_mut(index) else {
            tracing::warn!(target: "ternvale::usb", port, "portsc write to invalid port");
            return;
        };
        if value & regs::PORT_PR != 0 && p.connected {
            p.enabled = true;
            tracing::debug!(target: "ternvale::usb", port, "port reset complete");
        }
    }

    /// Allocate the first free slot for a port's device.
    pub fn enable_slot(&mut self) -> Option<u8> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if !slot.in_use() {
                let id = index + 1;
                // Prefer keyboard then mouse by port order.
                let port = self
                    .ports
                    .iter()
                    .find(|p| p.connected && !p.enabled)
                    .map(|p| (p.kind, p));
                if let Some((kind, _)) = port {
                    let port_num = kind.port();
                    slot.port = port_num;
                    slot.hid = Some(kind);
                    slot.address = kind.default_address();
                    self.ports[port_num as usize - 1].enabled = true;
                    tracing::info!(
                        target: "ternvale::usb",
                        slot_id = id,
                        port = port_num,
                        hid = ?kind,
                        "xhci slot enabled"
                    );
                    return Some(id as u8);
                }
                return None;
            }
        }
        None
    }

    /// Slot by id (1-based).
    pub fn slot_mut(&mut self, id: u8) -> Option<&mut Slot> {
        if id == 0 {
            return None;
        }
        self.slots.get_mut(id as usize - 1)
    }

    /// Free `slot_id` and mark its port available again.
    pub fn disable_slot(&mut self, slot_id: u8) -> bool {
        let Some(slot) = self.slot_mut(slot_id) else {
            return false;
        };
        if !slot.in_use() {
            return false;
        }
        let port = slot.port;
        *slot = Slot::default();
        if port > 0 && port as usize <= self.ports.len() {
            self.ports[port as usize - 1].enabled = false;
        }
        tracing::info!(target: "ternvale::usb", slot_id, port, "xhci slot freed");
        true
    }

    /// Slot id serving `port`, if enabled.
    pub fn slot_for_port(&self, port: u8) -> Option<u8> {
        for (index, slot) in self.slots.iter().enumerate() {
            if slot.port == port {
                return Some(index as u8 + 1);
            }
        }
        None
    }
}
