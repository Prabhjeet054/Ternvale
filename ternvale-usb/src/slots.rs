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
    /// Waiting on the passthrough worker for this ring.
    pub passthrough_busy: bool,
}

/// One device slot (xHCI slot id 1..=MAX_SLOTS).
#[derive(Debug, Clone)]
pub struct Slot {
    /// Zero when the slot is free.
    pub port: u8,
    /// USB device address.
    pub address: u8,
    /// Attached HID kind, if any (virtual devices only).
    pub hid: Option<HidKind>,
    /// When true, control/bulk transfers are forwarded to a host USB backend.
    pub passthrough: bool,
    /// EP0 default control pipe.
    pub ep0: EpRing,
    /// Interrupt IN (EP1) — legacy single-endpoint path for virtual HID.
    pub ep_in: EpRing,
    /// Per-DCI transfer rings (index = device context index).
    pub ep_by_dci: Vec<EpRing>,
    /// USB endpoint address per DCI (`0x81`, `0x02`, …).
    pub endpoint_addr: Vec<u8>,
    /// EP0 ring walk is waiting on the passthrough worker.
    pub ep0_passthrough_busy: bool,
    /// Pending IN report for HID interrupt endpoint.
    pub pending_in: Vec<u8>,
}

impl Default for Slot {
    fn default() -> Self {
        Self {
            port: 0,
            address: 0,
            hid: None,
            passthrough: false,
            ep0: EpRing::default(),
            ep_in: EpRing {
                max_packet: 8,
                ..EpRing::default()
            },
            ep_by_dci: vec![EpRing::default(); 32],
            endpoint_addr: vec![0u8; 32],
            ep0_passthrough_busy: false,
            pending_in: Vec::new(),
        }
    }
}

impl Slot {
    /// `true` when the slot is allocated.
    pub fn in_use(&self) -> bool {
        self.port != 0
    }

    /// Configure or replace the transfer ring for `dci`.
    pub fn set_endpoint_ring(
        &mut self,
        dci: u8,
        dequeue: u64,
        enabled: bool,
        endpoint: Option<u8>,
    ) {
        let index = dci as usize;
        if index < self.ep_by_dci.len() {
            self.ep_by_dci[index].dequeue = dequeue;
            self.ep_by_dci[index].cycle = true;
            self.ep_by_dci[index].enabled = enabled;
        }
        if let Some(addr) = endpoint {
            if index < self.endpoint_addr.len() {
                self.endpoint_addr[index] = addr;
            }
        }
        if dci == 1 {
            self.ep0.dequeue = dequeue;
            self.ep0.cycle = true;
            self.ep0.enabled = enabled;
        }
    }

    /// Transfer ring for `dci` (falls back to legacy `ep_in` for DCI 2).
    pub fn ep_ring(&self, dci: u8) -> &EpRing {
        if dci == 0 {
            return &self.ep0;
        }
        let index = dci as usize;
        if index < self.ep_by_dci.len() && self.ep_by_dci[index].enabled {
            return &self.ep_by_dci[index];
        }
        if dci == 2 {
            return &self.ep_in;
        }
        &self.ep0
    }

    /// Mutable transfer ring for `dci`.
    pub fn ep_ring_mut(&mut self, dci: u8) -> &mut EpRing {
        if dci == 0 {
            return &mut self.ep0;
        }
        let index = dci as usize;
        if index < self.ep_by_dci.len() && self.ep_by_dci[index].enabled {
            return &mut self.ep_by_dci[index];
        }
        if dci == 2 {
            return &mut self.ep_in;
        }
        &mut self.ep0
    }
}

/// Root port and slot allocator.
#[derive(Debug)]
pub struct SlotTable {
    ports: [Port; regs::NUM_PORTS as usize],
    slots: Vec<Slot>,
}

/// What is wired to a root port.
#[derive(Debug, Clone, Copy)]
pub enum PortKind {
    /// Virtual boot HID.
    Hid(HidKind),
    /// Host passthrough device (must be claimed before the VM starts).
    Passthrough {
        /// USB vendor ID.
        vendor_id: u16,
        /// USB product ID.
        product_id: u16,
    },
}

/// One root hub port.
#[derive(Debug, Clone, Copy)]
pub struct Port {
    /// Device connected (CCS).
    pub connected: bool,
    /// Port enabled.
    pub enabled: bool,
    /// Port role.
    pub kind: PortKind,
}

impl SlotTable {
    /// Ports start with keyboard and mouse connected.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
    pub fn new(max_slots: u8) -> Self {
        let ports = [
            Port {
                connected: true,
                enabled: false,
                kind: PortKind::Hid(HidKind::Keyboard),
            },
            Port {
                connected: true,
                enabled: false,
                kind: PortKind::Hid(HidKind::Mouse),
            },
            Port {
                connected: false,
                enabled: false,
                kind: PortKind::Passthrough {
                    vendor_id: 0,
                    product_id: 0,
                },
            },
            Port {
                connected: false,
                enabled: false,
                kind: PortKind::Passthrough {
                    vendor_id: 0,
                    product_id: 0,
                },
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

    /// Wire passthrough entries onto root ports 3+ (must be called before the guest enumerates).
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip(self, entries))]
    pub fn configure_passthrough_ports(&mut self, entries: &[(u8, u16, u16)]) {
        for (port, vid, pid) in entries {
            let index = *port as usize - 1;
            if index >= self.ports.len() {
                tracing::warn!(target: "ternvale::usb", port, "passthrough port out of range");
                continue;
            }
            self.ports[index] = Port {
                connected: true,
                enabled: false,
                kind: PortKind::Passthrough {
                    vendor_id: *vid,
                    product_id: *pid,
                },
            };
            tracing::info!(
                target: "ternvale::usb",
                port,
                vendor_id = format!("{vid:#06x}"),
                product_id = format!("{pid:#06x}"),
                "passthrough port configured"
            );
        }
    }

    /// Allocate the first free slot for a port's device.
    pub fn enable_slot(&mut self) -> Option<u8> {
        for (index, slot) in self.slots.iter_mut().enumerate() {
            if !slot.in_use() {
                let id = index + 1;
                let port_idx = self.ports.iter().position(|p| p.connected && !p.enabled);
                let port_idx = port_idx?;
                let port_num = port_idx + 1;
                let kind = self.ports[port_idx].kind;
                slot.port = port_num as u8;
                match kind {
                    PortKind::Hid(hid) => {
                        slot.hid = Some(hid);
                        slot.passthrough = false;
                        slot.address = hid.default_address();
                        tracing::info!(
                            target: "ternvale::usb",
                            slot_id = id,
                            port = port_num,
                            hid = ?hid,
                            "xhci slot enabled"
                        );
                    }
                    PortKind::Passthrough {
                        vendor_id,
                        product_id,
                    } => {
                        slot.hid = None;
                        slot.passthrough = true;
                        slot.address = id as u8;
                        tracing::info!(
                            target: "ternvale::usb",
                            slot_id = id,
                            port = port_num,
                            vendor_id = format!("{vendor_id:#06x}"),
                            product_id = format!("{product_id:#06x}"),
                            "xhci passthrough slot enabled"
                        );
                    }
                }
                self.ports[port_idx].enabled = true;
                return Some(id as u8);
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

    /// Guest unplug: mark `port` disconnected and free its slot.
    #[tracing::instrument(level = "warn", target = "ternvale::usb", skip(self), fields(port))]
    pub fn disconnect_passthrough_port(&mut self, port: u8) -> Option<u8> {
        let index = port as usize - 1;
        if index >= self.ports.len() {
            tracing::warn!(target: "ternvale::usb", port, "disconnect on invalid port");
            return None;
        }
        self.ports[index].connected = false;
        self.ports[index].enabled = false;
        let slot_id = self
            .slots
            .iter()
            .enumerate()
            .find(|(_, s)| s.port == port)
            .map(|(i, _)| i + 1);
        if let Some(id) = slot_id {
            let id = id as u8;
            self.disable_slot(id);
            tracing::warn!(
                target: "ternvale::usb",
                port,
                slot_id = id,
                "passthrough device disconnected from root port"
            );
            return Some(id);
        }
        tracing::warn!(target: "ternvale::usb", port, "passthrough disconnect with no active slot");
        None
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
