//! virtio-console worker: control plane plus the agent named port.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ternvale_vmm::GuestMemory;

use super::spec::{
    encode_port_name, Control, AGENT_PORT_ID, AGENT_PORT_NAME, AGENT_RX, AGENT_TX, CONTROL_RX,
    CONTROL_TX, DEVICE_READY, NUM_QUEUES, PORT_ADD, PORT_OPEN, PORT_READY,
};
use super::ConsoleStats;
use crate::virtio::irq::VirtioIrq;
use crate::virtio::queue::SplitQueue;
use crate::virtio::{QueueNotify, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};

const POLL: Duration = Duration::from_millis(10);
const INT_VRING: u32 = 1;

pub(super) enum Kick {
    Notify(QueueNotify),
    Reset,
}

struct Bound {
    key: (u16, u64, u64, u64),
    queue: SplitQueue,
}

pub(super) struct Worker {
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
    stats: Arc<ConsoleStats>,
    host: PathBuf,
    queues: [Option<Bound>; NUM_QUEUES as usize],
    pending: VecDeque<Vec<u8>>,
    socket: Option<UnixStream>,
    guest_open: bool,
    announced: bool,
}

impl Worker {
    pub(super) fn new(
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        stats: Arc<ConsoleStats>,
        host: PathBuf,
    ) -> Self {
        Self {
            memory,
            irq,
            stats,
            host,
            queues: std::array::from_fn(|_| None),
            pending: VecDeque::new(),
            socket: None,
            guest_open: false,
            announced: false,
        }
    }

    pub(super) fn run(mut self, kicks: Receiver<Kick>) {
        tracing::debug!(
            target: "ternvale::virtio::console",
            host = %self.host.display(),
            "virtio-console worker started"
        );
        loop {
            match kicks.recv_timeout(POLL) {
                Ok(Kick::Notify(notify)) => self.on_notify(notify),
                Ok(Kick::Reset) => self.reset(),
                Err(RecvTimeoutError::Timeout) => self.pump(),
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        tracing::debug!(target: "ternvale::virtio::console", "virtio-console worker stopped");
    }

    fn on_notify(&mut self, notify: QueueNotify) {
        if notify.index >= NUM_QUEUES {
            tracing::warn!(
                target: "ternvale::virtio::console",
                queue = notify.index,
                "notify for a queue virtio-console does not have"
            );
            return;
        }
        self.bind(notify.index, &notify);
        self.pump();
    }

    fn bind(&mut self, index: u16, notify: &QueueNotify) {
        let key = (notify.size, notify.desc, notify.avail, notify.used);
        let slot = &mut self.queues[index as usize];
        if slot.as_ref().is_some_and(|bound| bound.key == key) {
            return;
        }
        tracing::debug!(
            target: "ternvale::virtio::console",
            queue = index,
            size = notify.size,
            desc = %format!("{:#x}", notify.desc),
            "virtio-console queue bound"
        );
        *slot = Some(Bound {
            key,
            queue: SplitQueue::new(
                notify.size,
                notify.desc,
                notify.avail,
                notify.used,
                notify.features & VIRTIO_F_INDIRECT_DESC != 0,
                notify.features & VIRTIO_F_EVENT_IDX != 0,
            ),
        });
    }

    fn reset(&mut self) {
        tracing::info!(target: "ternvale::virtio::console", "virtio-console reset");
        self.queues = std::array::from_fn(|_| None);
        self.pending.clear();
        self.socket = None;
        self.guest_open = false;
        self.announced = false;
    }

    fn pump(&mut self) {
        self.control_rx();
        self.ensure_socket();
        self.control_tx();
        self.agent_tx();
        self.agent_rx();
    }

    fn control_rx(&mut self) {
        let mut msgs = Vec::new();
        let mut raised = false;
        {
            let Some(bound) = self.queues[CONTROL_RX as usize].as_mut() else {
                return;
            };
            let Ok(mem) = self.memory.lock() else {
                return;
            };
            let chains: Vec<_> = bound.queue.chains(&mem).collect();
            drop(mem);
            for chain in chains {
                let mut payload = Vec::new();
                if let Ok(mut mem) = self.memory.lock() {
                    for buf in &chain.readable {
                        let mut chunk = vec![0u8; buf.len as usize];
                        if mem.read_bytes(buf.addr, &mut chunk).is_ok() {
                            payload.extend_from_slice(&chunk);
                        }
                    }
                    bound.queue.add_used(&mut mem, chain.head, 0);
                    raised = true;
                }
                if let Some(msg) = Control::decode(&payload) {
                    tracing::debug!(
                        target: "ternvale::virtio::console",
                        event = msg.event_name(),
                        id = msg.id,
                        value = msg.value,
                        "console control from guest"
                    );
                    msgs.push(msg);
                }
            }
        }
        for msg in msgs {
            self.on_control(msg);
        }
        if raised {
            self.irq.raise(INT_VRING);
        }
    }

    fn on_control(&mut self, msg: Control) {
        match msg.event {
            DEVICE_READY if msg.value == 1 => self.announce_ports(),
            PORT_READY if msg.id == AGENT_PORT_ID && msg.value == 1 => {
                tracing::info!(
                    target: "ternvale::virtio::console",
                    name = AGENT_PORT_NAME,
                    "guest ready on agent serial port"
                );
            }
            PORT_OPEN if msg.id == AGENT_PORT_ID => {
                self.guest_open = msg.value == 1;
                if !self.guest_open {
                    self.socket = None;
                }
                tracing::info!(
                    target: "ternvale::virtio::console",
                    open = self.guest_open,
                    "agent serial port open"
                );
            }
            _ => {}
        }
    }

    fn announce_ports(&mut self) {
        if self.announced {
            return;
        }
        self.announced = true;
        self.pending.push_back(
            Control {
                id: AGENT_PORT_ID,
                event: PORT_ADD,
                value: 1,
            }
            .encode()
            .to_vec(),
        );
        self.pending
            .push_back(encode_port_name(AGENT_PORT_ID, AGENT_PORT_NAME));
        tracing::info!(
            target: "ternvale::virtio::console",
            name = AGENT_PORT_NAME,
            "announced agent serial port"
        );
    }

    fn control_tx(&mut self) {
        while let Some(msg) = self.pending.pop_front() {
            if !self.write_chain(CONTROL_TX, &msg) {
                self.pending.push_front(msg);
                break;
            }
        }
    }

    fn ensure_socket(&mut self) {
        if !self.guest_open || self.socket.is_some() {
            return;
        }
        match UnixStream::connect(&self.host) {
            Ok(stream) => {
                if let Err(error) = stream.set_nonblocking(true) {
                    tracing::warn!(
                        target: "ternvale::virtio::console",
                        error = %error,
                        "agent serial host socket set_nonblocking failed"
                    );
                }
                tracing::info!(
                    target: "ternvale::virtio::console",
                    path = %self.host.display(),
                    "agent serial connected to host socket"
                );
                self.stats.connections.fetch_add(1, Ordering::Relaxed);
                self.socket = Some(stream);
            }
            Err(error) if error.kind() == ErrorKind::ConnectionRefused => {}
            Err(error) => {
                tracing::debug!(
                    target: "ternvale::virtio::console",
                    path = %self.host.display(),
                    error = %error,
                    "agent serial host socket not ready"
                );
            }
        }
    }

    fn agent_tx(&mut self) {
        let Some(bound) = self.queues[AGENT_TX as usize].as_mut() else {
            return;
        };
        let Ok(mem) = self.memory.lock() else {
            return;
        };
        let chains: Vec<_> = bound.queue.chains(&mem).collect();
        drop(mem);
        let mut raised = false;
        for chain in chains {
            let mut payload = Vec::new();
            if let Ok(mem) = self.memory.lock() {
                for buf in &chain.readable {
                    let mut chunk = vec![0u8; buf.len as usize];
                    if mem.read_bytes(buf.addr, &mut chunk).is_ok() {
                        payload.extend_from_slice(&chunk);
                    }
                }
            }
            if let Some(socket) = self.socket.as_mut() {
                if let Err(error) = socket.write_all(&payload) {
                    tracing::debug!(
                        target: "ternvale::virtio::console",
                        error = %error,
                        "agent serial host write failed"
                    );
                    self.socket = None;
                } else {
                    self.stats
                        .tx_bytes
                        .fetch_add(payload.len() as u64, Ordering::Relaxed);
                }
            }
            if let Ok(mut mem) = self.memory.lock() {
                bound.queue.add_used(&mut mem, chain.head, 0);
                raised = true;
            }
        }
        if raised {
            self.irq.raise(INT_VRING);
        }
    }

    fn agent_rx(&mut self) {
        let Some(socket) = self.socket.as_mut() else {
            return;
        };
        let mut buf = [0u8; 4096];
        let n = match socket.read(&mut buf) {
            Ok(0) => {
                self.socket = None;
                return;
            }
            Ok(n) => n,
            Err(error) if error.kind() == ErrorKind::WouldBlock => return,
            Err(error) => {
                tracing::debug!(
                    target: "ternvale::virtio::console",
                    error = %error,
                    "agent serial host read failed"
                );
                self.socket = None;
                return;
            }
        };
        if self.write_chain(AGENT_RX, &buf[..n]) {
            self.stats.rx_bytes.fetch_add(n as u64, Ordering::Relaxed);
        }
    }

    fn write_chain(&mut self, index: u16, data: &[u8]) -> bool {
        let Some(bound) = self.queues[index as usize].as_mut() else {
            return false;
        };
        let Ok(mut mem) = self.memory.lock() else {
            return false;
        };
        let Some(chain) = bound.queue.chains(&mem).next() else {
            return false;
        };
        let mut written = 0usize;
        for buf in &chain.writable {
            if written >= data.len() {
                break;
            }
            let take = (buf.len as usize).min(data.len() - written);
            if mem
                .write_bytes(buf.addr, &data[written..written + take])
                .is_err()
            {
                break;
            }
            written += take;
        }
        bound.queue.add_used(&mut mem, chain.head, written as u32);
        drop(mem);
        self.irq.raise(INT_VRING);
        written == data.len() || index == AGENT_RX
    }
}
