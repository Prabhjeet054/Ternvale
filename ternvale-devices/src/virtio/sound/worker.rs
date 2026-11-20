//! virtio-sound worker: control, TX playback, event notifications.

use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use std::sync::atomic::{AtomicBool, Ordering};

use ternvale_audio_host::{
    open_default_capture, open_default_playback, CaptureSource, PlaybackSink, SampleFormat,
};
use ternvale_vmm::GuestMemory;

use super::protocol::{
    encode_pcm_event, encode_pcm_info_range, parse_set_params, parse_stream_id, write_status,
    PcmParams, EVT_PCM_PERIOD_ELAPSED, EVT_PCM_XRUN, R_PCM_INFO, R_PCM_PREPARE, R_PCM_RELEASE,
    R_PCM_SET_PARAMS, R_PCM_START, R_PCM_STOP, STREAM_CAPTURE, STREAM_PLAYBACK, S_BAD_MSG,
    S_IO_ERR, S_NOT_SUPP, S_OK, VQ_CONTROL, VQ_EVENT, VQ_RX, VQ_TX,
};
use crate::virtio::irq::VirtioIrq;
use crate::virtio::queue::{Chain, SplitQueue};
use crate::virtio::{QueueNotify, VIRTIO_F_EVENT_IDX, VIRTIO_F_INDIRECT_DESC};

const POLL: Duration = Duration::from_millis(5);
const INT_VRING: u32 = 1;

pub(super) enum Kick {
    Notify(QueueNotify),
    Reset,
}

struct Bound {
    queue: SplitQueue,
}

pub(super) struct Worker {
    memory: Arc<Mutex<GuestMemory>>,
    irq: Arc<VirtioIrq>,
    playback: Box<dyn PlaybackSink>,
    capture: Option<Box<dyn CaptureSource>>,
    capturing: Arc<AtomicBool>,
    streams: u32,
    control: Option<Bound>,
    event: Option<Bound>,
    tx: Option<Bound>,
    rx: Option<Bound>,
    playback_params: Option<PcmParams>,
    capture_params: Option<PcmParams>,
    running_tx: bool,
    running_rx: bool,
    bytes_since_period: u32,
    pending_events: Vec<Chain>,
}

impl Worker {
    pub(super) fn new(
        memory: Arc<Mutex<GuestMemory>>,
        irq: Arc<VirtioIrq>,
        capture: bool,
        capturing: Arc<AtomicBool>,
    ) -> Self {
        Self {
            memory,
            irq,
            playback: open_default_playback(),
            capture: capture.then(open_default_capture),
            capturing,
            streams: if capture { 2 } else { 1 },
            control: None,
            event: None,
            tx: None,
            rx: None,
            playback_params: None,
            capture_params: None,
            running_tx: false,
            running_rx: false,
            bytes_since_period: 0,
            pending_events: Vec::new(),
        }
    }

    pub(super) fn run(mut self, kicks: Receiver<Kick>) {
        tracing::debug!(target: "ternvale::virtio::sound", "virtio-sound worker started");
        loop {
            match kicks.recv_timeout(POLL) {
                Ok(Kick::Notify(notify)) => self.bind(&notify),
                Ok(Kick::Reset) => self.reset(),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => break,
            }
            self.drain_queues();
        }
        self.playback.close();
        tracing::info!(target: "ternvale::virtio::sound", "virtio-sound worker stopped");
    }

    fn reset(&mut self) {
        self.control = None;
        self.event = None;
        self.tx = None;
        self.rx = None;
        self.playback_params = None;
        self.capture_params = None;
        self.running_tx = false;
        self.running_rx = false;
        self.bytes_since_period = 0;
        self.pending_events.clear();
        self.playback.close();
        if let Some(capture) = self.capture.as_mut() {
            capture.close();
        }
        self.capturing.store(false, Ordering::Relaxed);
    }

    fn bind(&mut self, notify: &QueueNotify) {
        let indirect = notify.features & VIRTIO_F_INDIRECT_DESC != 0;
        let event_idx = notify.features & VIRTIO_F_EVENT_IDX != 0;
        let queue = SplitQueue::new(
            notify.size,
            notify.desc,
            notify.avail,
            notify.used,
            indirect,
            event_idx,
        );
        let bound = Bound { queue };
        match notify.index {
            VQ_CONTROL => self.control = Some(bound),
            VQ_EVENT => self.event = Some(bound),
            VQ_TX => self.tx = Some(bound),
            VQ_RX => self.rx = Some(bound),
            _ => tracing::warn!(
                target: "ternvale::virtio::sound",
                queue = notify.index,
                "unknown virtio-sound queue"
            ),
        }
    }

    fn drain_queues(&mut self) {
        if self.control.is_some() {
            let mut bound = self.control.take().expect("control");
            self.process_control(&mut bound);
            self.control = Some(bound);
        }
        if self.event.is_some() {
            let mut bound = self.event.take().expect("event");
            self.collect_events(&mut bound);
            self.event = Some(bound);
        }
        if self.tx.is_some() {
            let mut bound = self.tx.take().expect("tx");
            self.process_tx(&mut bound);
            self.tx = Some(bound);
        }
        if self.rx.is_some() {
            let mut bound = self.rx.take().expect("rx");
            self.process_rx(&mut bound);
            self.rx = Some(bound);
        }
    }

    fn with_mem<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&GuestMemory) -> R,
    {
        self.memory.lock().ok().map(|guard| f(&guard))
    }

    fn with_mem_mut<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut GuestMemory) -> R,
    {
        self.memory.lock().ok().map(|mut guard| f(&mut guard))
    }

    fn process_control(&mut self, bound: &mut Bound) {
        let Some(chains) = self.with_mem(|mem| bound.queue.chains(mem).collect::<Vec<_>>()) else {
            return;
        };
        let mut irq = false;
        for chain in chains {
            let status = self.handle_control(&chain);
            let ok = self.with_mem_mut(|mem| {
                write_control_response(mem, &chain, status, self.streams)
                    && bound.queue.add_used(mem, chain.head, 0)
            });
            if ok == Some(true) {
                irq = true;
            }
        }
        if irq {
            self.maybe_notify(bound);
        }
    }

    fn handle_control(&mut self, chain: &Chain) -> u32 {
        let req = self
            .with_mem(|mem| read_readable(mem, chain))
            .unwrap_or_default();
        if req.len() < 4 {
            return S_BAD_MSG;
        }
        let code = u32::from_le_bytes(req[0..4].try_into().expect("code"));
        tracing::trace!(
            target: "ternvale::virtio::sound",
            code = format!("{code:#x}"),
            "virtio-sound control"
        );
        match code {
            R_PCM_INFO => {
                let start = if req.len() >= 8 {
                    u32::from_le_bytes(req[4..8].try_into().expect("start"))
                } else {
                    0
                };
                if start >= self.streams {
                    return S_BAD_MSG;
                }
                S_OK
            }
            R_PCM_SET_PARAMS => self.handle_set_params(&req),
            R_PCM_PREPARE => S_OK,
            _ if code == R_PCM_START => {
                match parse_stream_id(&req) {
                    Some(STREAM_CAPTURE) if self.capture.is_some() => self.running_rx = true,
                    _ => self.running_tx = true,
                }
                S_OK
            }
            _ if code == R_PCM_STOP => {
                match parse_stream_id(&req) {
                    Some(STREAM_CAPTURE) => {
                        self.running_rx = false;
                        self.capturing.store(false, Ordering::Relaxed);
                    }
                    _ => self.running_tx = false,
                }
                S_OK
            }
            _ if code == R_PCM_RELEASE => {
                match parse_stream_id(&req) {
                    Some(STREAM_CAPTURE) => {
                        self.running_rx = false;
                        self.capture_params = None;
                        if let Some(capture) = self.capture.as_mut() {
                            capture.close();
                        }
                        self.capturing.store(false, Ordering::Relaxed);
                    }
                    _ => {
                        self.running_tx = false;
                        self.playback.close();
                        self.playback_params = None;
                    }
                }
                S_OK
            }
            _ => S_NOT_SUPP,
        }
    }

    fn handle_set_params(&mut self, req: &[u8]) -> u32 {
        let Some(params) = parse_set_params(req) else {
            return S_BAD_MSG;
        };
        if params.channels == 0 || params.channels > 2 {
            return S_BAD_MSG;
        }
        tracing::info!(
            target: "ternvale::virtio::sound",
            stream = params.stream_id,
            format = %params.describe(),
            "virtio-sound PCM format negotiated"
        );
        let host_fmt = SampleFormat {
            rate_hz: params.rate_hz(),
            channels: params.channels,
            bytes_per_frame: params.bytes_per_frame(),
        };
        if params.stream_id == STREAM_CAPTURE {
            if params.stream_id >= self.streams {
                return S_BAD_MSG;
            }
            let Some(capture) = self.capture.as_mut() else {
                return S_NOT_SUPP;
            };
            if capture.configure(host_fmt).is_err() {
                return S_IO_ERR;
            }
            self.capture_params = Some(params);
        } else {
            if self.playback.configure(host_fmt).is_err() {
                return S_IO_ERR;
            }
            self.playback_params = Some(params);
            self.bytes_since_period = 0;
        }
        S_OK
    }

    fn collect_events(&mut self, bound: &mut Bound) {
        let mut mem = match self.memory.lock() {
            Ok(guard) => guard,
            Err(_) => return,
        };
        let chains: Vec<Chain> = bound.queue.chains(&mem).collect();
        let mut irq = false;
        for chain in chains {
            irq |= bound.queue.add_used(&mut mem, chain.head, 0);
            self.pending_events.push(chain);
        }
        if irq && bound.queue.notification_needed(&mem) {
            self.irq.raise(INT_VRING);
        }
    }

    fn process_tx(&mut self, bound: &mut Bound) {
        if !self.running_tx {
            return;
        }
        let Some(chains) = self.with_mem(|mem| bound.queue.chains(mem).collect::<Vec<_>>()) else {
            return;
        };
        let mut irq = false;
        for chain in chains {
            let result = self.play_tx(&chain);
            let ok = self.with_mem_mut(|mem| match result {
                Ok(bytes) => bound.queue.add_used(mem, chain.head, bytes),
                Err(status) => {
                    write_tx_status(mem, &chain, status).is_ok()
                        && bound.queue.add_used(mem, chain.head, 0)
                }
            });
            if ok == Some(true) {
                irq = true;
            }
        }
        if irq {
            self.maybe_notify(bound);
        }
    }

    fn play_tx(&mut self, chain: &Chain) -> Result<u32, u32> {
        let readable = chain.readable.first().ok_or(S_BAD_MSG)?;
        let mut hdr = [0u8; 4];
        self.with_mem(|mem| mem.read_bytes(readable.addr, &mut hdr))
            .ok_or(S_IO_ERR)?
            .map_err(|_| S_IO_ERR)?;
        let stream = u32::from_le_bytes(hdr);
        if stream != STREAM_PLAYBACK {
            return Err(S_BAD_MSG);
        }
        let pcm_len = readable.len.saturating_sub(4);
        let mut pcm = vec![0u8; pcm_len as usize];
        if pcm_len > 0 {
            self.with_mem(|mem| mem.read_bytes(readable.addr + 4, &mut pcm))
                .ok_or(S_IO_ERR)?
                .map_err(|_| S_IO_ERR)?;
        }
        if self.playback.push_pcm(&pcm).is_err() {
            tracing::warn!(
                target: "ternvale::virtio::sound",
                event_code = EVT_PCM_XRUN,
                "virtio-sound host playback rejected PCM"
            );
            return Err(S_IO_ERR);
        }
        write_tx_status_chain(self, chain, S_OK)?;
        if let Some(params) = self.playback_params {
            self.bytes_since_period += pcm_len;
            if params.period_bytes > 0 && self.bytes_since_period >= params.period_bytes {
                self.bytes_since_period -= params.period_bytes;
                self.post_period_event(STREAM_PLAYBACK);
            }
        }
        Ok(pcm_len)
    }

    fn post_period_event(&mut self, stream: u32) {
        let Some(chain) = self.pending_events.pop() else {
            return;
        };
        let event = encode_pcm_event(EVT_PCM_PERIOD_ELAPSED, stream);
        let delivered = self.with_mem_mut(|mem| {
            chain
                .writable
                .first()
                .is_some_and(|w| mem.write_bytes(w.addr, &event).is_ok())
        });
        if delivered != Some(true) {
            self.pending_events.push(chain);
        }
    }

    fn process_rx(&mut self, bound: &mut Bound) {
        if self.capture.is_none() {
            self.reject_rx(bound);
            return;
        }
        if !self.running_rx {
            return;
        }
        let Some(chains) = self.with_mem(|mem| bound.queue.chains(mem).collect::<Vec<_>>()) else {
            return;
        };
        let mut irq = false;
        for chain in chains {
            let filled = self.fill_rx(&chain);
            let ok = self.with_mem_mut(|mem| bound.queue.add_used(mem, chain.head, filled));
            if ok == Some(true) {
                irq = true;
            }
        }
        if irq {
            self.maybe_notify(bound);
        }
        if let Some(capture) = self.capture.as_ref() {
            self.capturing.store(capture.capturing(), Ordering::Relaxed);
        }
    }

    fn reject_rx(&mut self, bound: &mut Bound) {
        let Some(chains) = self.with_mem(|mem| bound.queue.chains(mem).collect::<Vec<_>>()) else {
            return;
        };
        let mut irq = false;
        for chain in chains {
            let ok = self.with_mem_mut(|mem| {
                if let Some(w) = chain.writable.first() {
                    let mut buf = [0u8; 8];
                    write_status(&mut buf, S_NOT_SUPP);
                    return mem.write_bytes(w.addr, &buf).is_ok()
                        && bound.queue.add_used(mem, chain.head, 0);
                }
                false
            });
            if ok == Some(true) {
                irq = true;
            }
        }
        if irq {
            self.maybe_notify(bound);
        }
    }

    fn fill_rx(&mut self, chain: &Chain) -> u32 {
        let Some(w) = chain.writable.first() else {
            return 0;
        };
        let room = w.len.saturating_sub(8) as usize;
        if room == 0 {
            return 0;
        }
        let mut pcm = vec![0u8; room];
        let n = self
            .capture
            .as_mut()
            .and_then(|c| c.pull_pcm(&mut pcm).ok())
            .unwrap_or(0);
        // Do not log `pcm`.
        let _ = self.with_mem_mut(|mem| {
            if n > 0 {
                let _ = mem.write_bytes(w.addr, &pcm[..n]);
            }
            let mut st = [0u8; 8];
            write_status(&mut st, S_OK);
            mem.write_bytes(w.addr + u64::from(w.len.saturating_sub(8)), &st)
        });
        n as u32
    }

    fn maybe_notify(&self, bound: &mut Bound) {
        let notify = self.with_mem_mut(|mem| bound.queue.notification_needed(mem));
        if notify == Some(true) {
            self.irq.raise(INT_VRING);
        }
    }
}

fn read_readable(mem: &GuestMemory, chain: &Chain) -> Vec<u8> {
    let mut out = Vec::new();
    for buf in &chain.readable {
        let mut chunk = vec![0u8; buf.len as usize];
        if mem.read_bytes(buf.addr, &mut chunk).is_ok() {
            out.extend(chunk);
        }
    }
    out
}

fn write_control_response(mem: &mut GuestMemory, chain: &Chain, status: u32, streams: u32) -> bool {
    if status == S_OK {
        let req = read_readable(mem, chain);
        let code = u32::from_le_bytes(req.get(0..4).unwrap_or(&[0; 4]).try_into().expect("c"));
        if code == R_PCM_INFO {
            let start = le_u32(req.get(4..8)).unwrap_or(0);
            let count = le_u32(req.get(8..12)).unwrap_or(1);
            let info = encode_pcm_info_range(start, count.max(1), streams);
            let w = chain.writable.first();
            return w.is_some_and(|w| mem.write_bytes(w.addr, &info).is_ok());
        }
    }
    let w = chain.writable.first();
    w.is_some_and(|w| {
        let mut buf = [0u8; 4];
        write_status(&mut buf, status);
        mem.write_bytes(w.addr, &buf).is_ok()
    })
}

fn write_tx_status_chain(worker: &Worker, chain: &Chain, status: u32) -> Result<(), u32> {
    worker
        .with_mem_mut(|mem| write_tx_status(mem, chain, status))
        .ok_or(S_IO_ERR)?
}

fn le_u32(slice: Option<&[u8]>) -> Option<u32> {
    let bytes = slice?;
    if bytes.len() < 4 {
        return None;
    }
    let mut raw = [0u8; 4];
    raw.copy_from_slice(&bytes[..4]);
    Some(u32::from_le_bytes(raw))
}

fn write_tx_status(mem: &mut GuestMemory, chain: &Chain, status: u32) -> Result<(), u32> {
    let w = chain.writable.first().ok_or(S_BAD_MSG)?;
    let mut buf = [0u8; 8];
    write_status(&mut buf, status);
    mem.write_bytes(w.addr, &buf).map_err(|_| S_IO_ERR)?;
    Ok(())
}
