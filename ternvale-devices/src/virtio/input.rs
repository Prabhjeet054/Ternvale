//! virtio-input (device id 18, virtio 1.2 §5.8): a keyboard or an absolute
//! (tablet) pointer.
//!
//! Queue 0 (eventq) carries 8-byte `virtio_input_event`s to the guest. The
//! host fills it through an [`InputSink`] from its own thread, never the vCPU.
//! Queue 1 (statusq) carries guest events such as keyboard LEDs; they are
//! logged and completed. Only `VIRTIO_F_VERSION_1` is offered.

mod codes;
mod config;
mod keymap;
mod sink;

use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use ternvale_vmm::pci::Bdf;
use ternvale_vmm::{DeviceAttach, GuestMemory, MachineError};

use super::irq::{IrqHook, VirtioIrq};
use super::queue::SplitQueue;
use super::{QueueNotify, VirtioDevice, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};
pub use codes::{
    type_name as input_type_name, InputEvent, ABS_MAX_VALUE, ABS_X, ABS_Y, BTN_LEFT, BTN_MIDDLE,
    BTN_RIGHT, EV_ABS, EV_KEY, EV_REL, EV_REP, EV_SYN, REL_HWHEEL, REL_WHEEL, SYN_REPORT,
};
pub use config::InputKind;
pub use config::{
    CFG_ABS_INFO, CFG_EV_BITS, CFG_ID_DEVIDS, CFG_ID_NAME, CFG_ID_SERIAL, CFG_PROP_BITS, CFG_UNSET,
};
use config::{CONFIG_LEN, UNION_LEN, UNION_OFFSET};
pub use keymap::linux_key as mac_to_linux_key;
pub use sink::{InputSink, InputStats, MAX_PENDING};
use sink::{QueueKey, Shared, State};

/// Virtio input device id.
pub const VIRTIO_INPUT_ID: u32 = 18;
/// Event queue index.
pub const INPUT_EVENTQ: u16 = 0;
/// Status queue index.
pub const INPUT_STATUSQ: u16 = 1;

const INT_VRING: u32 = 1;

/// Building a virtio-input device failed.
#[derive(Debug, thiserror::Error)]
pub enum InputError {
    /// The virtio-mmio slot is outside the platform window.
    #[error("virtio-input slot {slot} is outside the virtio-mmio window")]
    BadSlot {
        /// Requested slot.
        slot: u32,
    },
}

/// A virtio-input device on virtio-mmio from [`VirtioInput::attach`].
pub struct AttachedInput {
    /// MMIO base.
    pub base: u64,
    /// MMIO window size.
    pub size: u64,
    /// The transport to register on the bus.
    pub device: Box<dyn ternvale_vmm::MmioDevice>,
    /// Event injection handle.
    pub sink: InputSink,
}

/// virtio-input device state (the vCPU side).
pub struct VirtioInput {
    shared: Arc<Shared>,
    select: u8,
    subsel: u8,
    statusq: Option<(QueueKey, SplitQueue)>,
}

impl VirtioInput {
    /// `kind` device over `memory` that raises `irq` when it fills eventq.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::input", skip(memory, irq))]
    pub fn new(kind: InputKind, memory: Arc<Mutex<GuestMemory>>, irq: Arc<VirtioIrq>) -> Self {
        tracing::info!(
            target: "ternvale::virtio::input",
            device = kind.short(),
            name = kind.name(),
            "virtio-input device ready"
        );
        Self {
            shared: Arc::new(Shared {
                kind,
                memory,
                irq,
                stats: Arc::default(),
                state: Mutex::new(State::default()),
            }),
            select: 0,
            subsel: 0,
            statusq: None,
        }
    }

    /// Event injection handle.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::input", skip_all)]
    pub fn sink(&self) -> InputSink {
        InputSink::new(&self.shared)
    }

    /// Build a virtio-mmio transport on `slot`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::virtio::input",
        skip(memory, irq_hook)
    )]
    pub fn attach(
        slot: u32,
        kind: InputKind,
        memory: Arc<Mutex<GuestMemory>>,
        irq_hook: IrqHook,
    ) -> Result<AttachedInput, InputError> {
        let base = super::slot_base(slot).ok_or_else(|| {
            tracing::error!(target: "ternvale::virtio::input", slot, "virtio-input slot rejected");
            InputError::BadSlot { slot }
        })?;
        let irq = VirtioIrq::new();
        irq.set_hook(irq_hook);
        let input = Self::new(kind, memory, Arc::clone(&irq));
        let sink = input.sink();
        let transport = super::mmio::VirtioMmio::with_irq(slot, Box::new(input), irq);
        Ok(AttachedInput {
            base,
            size: ternvale_vmm::VIRTIO_MMIO_SLOT_SIZE,
            device: Box::new(transport),
            sink,
        })
    }

    /// Put a `kind` device on the next free virtio-pci function of bus 0.
    #[tracing::instrument(level = "debug", target = "ternvale::virtio::input", skip(attach))]
    pub fn attach_pci(
        attach: &DeviceAttach,
        kind: InputKind,
    ) -> Result<(Bdf, InputSink), MachineError> {
        let irq = VirtioIrq::new();
        let input = Self::new(kind, Arc::clone(&attach.memory), Arc::clone(&irq));
        let sink = input.sink();
        let bdf = super::pci::attach_pci(attach, Box::new(input), irq)?;
        tracing::info!(target: "ternvale::virtio::input", %bdf, device = kind.short(), "virtio-input on virtio-pci");
        Ok((bdf, sink))
    }

    fn bind(slot: &mut Option<(QueueKey, SplitQueue)>, notify: &QueueNotify, device: &str) {
        let key = (notify.size, notify.desc, notify.avail, notify.used);
        if slot.as_ref().is_some_and(|(bound, _)| *bound == key) {
            return;
        }
        tracing::debug!(
            target: "ternvale::virtio::input",
            device,
            queue = notify.index,
            size = notify.size,
            desc = %format!("{:#x}", notify.desc),
            "virtio-input queue bound"
        );
        *slot = Some((
            key,
            SplitQueue::new(
                notify.size,
                notify.desc,
                notify.avail,
                notify.used,
                notify.features & VIRTIO_F_INDIRECT_DESC != 0,
                notify.features & VIRTIO_F_EVENT_IDX != 0,
            ),
        ));
    }

    fn serve_status(&mut self) {
        let Some((_, queue)) = self.statusq.as_mut() else {
            return;
        };
        let device = self.shared.kind.short();
        let mut mem = ternvale_vmm::lockwatch::lock(&self.shared.memory, "guest-memory");
        let mut completed = false;
        loop {
            let next = queue.chains(&mem).next();
            let Some(chain) = next else {
                break;
            };
            let mut bytes = [0u8; 8];
            let read = chain
                .readable
                .first()
                .filter(|b| b.len >= 8)
                .is_some_and(|b| mem.read_bytes(b.addr, &mut bytes).is_ok());
            if read {
                let event = InputEvent::from_bytes(bytes);
                self.shared.stats.status.fetch_add(1, Ordering::Relaxed);
                tracing::debug!(
                    target: "ternvale::virtio::input",
                    device,
                    ev_type = codes::type_name(event.ev_type),
                    code = %format!("{:#x}", event.code),
                    value = event.value,
                    "virtio-input status event from guest"
                );
            } else {
                tracing::warn!(
                    target: "ternvale::virtio::input",
                    device,
                    head = chain.head,
                    "virtio-input status buffer is not one readable 8-byte buffer in guest memory; ignored"
                );
            }
            completed |= queue.add_used(&mut mem, chain.head, 0);
        }
        if completed && queue.notification_needed(&mem) {
            self.shared.irq.raise(INT_VRING);
        }
    }
}

impl VirtioDevice for VirtioInput {
    fn device_id(&self) -> u32 {
        VIRTIO_INPUT_ID
    }

    fn num_queues(&self) -> u16 {
        2
    }

    fn queue_num_max(&self, _queue: u16) -> u16 {
        64
    }

    fn read_config(&mut self, offset: u64, size: u8) -> u64 {
        let payload = config::payload(self.shared.kind, self.select, self.subsel);
        let mut value = 0u64;
        for i in 0..u64::from(size.min(8)) {
            let at = offset + i;
            let byte = match at {
                0 => self.select,
                1 => self.subsel,
                // payload is at most UNION_LEN bytes.
                2 => payload.len().min(UNION_LEN) as u8,
                _ if (UNION_OFFSET..CONFIG_LEN).contains(&at) => payload
                    .get((at - UNION_OFFSET) as usize)
                    .copied()
                    .unwrap_or(0),
                _ => 0,
            };
            value |= u64::from(byte) << (8 * i);
        }
        tracing::trace!(
            target: "ternvale::virtio::input",
            device = self.shared.kind.short(),
            offset = %format!("{:#x}", offset),
            size,
            value = %format!("{:#x}", value),
            "virtio-input config read"
        );
        value
    }

    fn write_config(&mut self, offset: u64, size: u8, value: u64) {
        let device = self.shared.kind.short();
        // Byte-wide writes to select/subsel; a 2-byte write covers both.
        for i in 0..u64::from(size.min(8)) {
            let byte = (value >> (8 * i)) as u8;
            match offset + i {
                0 => self.select = byte,
                1 => self.subsel = byte,
                at => {
                    tracing::warn!(
                        target: "ternvale::virtio::input",
                        device,
                        offset = %format!("{:#x}", at),
                        "write to read-only virtio-input config ignored"
                    );
                    return;
                }
            }
        }
        let size = config::payload(self.shared.kind, self.select, self.subsel).len();
        tracing::debug!(
            target: "ternvale::virtio::input",
            device,
            select = %format!("{:#x}", self.select),
            subsel = %format!("{:#x}", self.subsel),
            size,
            "virtio-input config select"
        );
    }

    fn notify(&mut self, notify: QueueNotify) {
        let device = self.shared.kind.short();
        match notify.index {
            INPUT_EVENTQ => {
                let shared = Arc::clone(&self.shared);
                let mut state = shared.lock_state();
                Self::bind(&mut state.eventq, &notify, device);
                shared.flush(&mut state);
            }
            INPUT_STATUSQ => {
                Self::bind(&mut self.statusq, &notify, device);
                self.serve_status();
            }
            other => tracing::warn!(
                target: "ternvale::virtio::input",
                device,
                queue = other,
                "notify for a queue virtio-input does not have"
            ),
        }
    }

    fn status_changed(&mut self, status: u32) {
        if status == 0 {
            self.statusq = None;
            self.select = 0;
            self.subsel = 0;
            self.shared.reset();
        }
    }
}

#[cfg(test)]
#[path = "input_tests.rs"]
mod tests;
