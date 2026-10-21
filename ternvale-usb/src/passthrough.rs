//! Passthrough slots: forward guest USB transfers to a [`HostUsbBackend`] on a worker thread.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use crate::event::{CC_STALL_ERROR, CC_SUCCESS};
use crate::guest_mem::{GuestMemoryAccess, MemError};
use crate::host::{HostUsbBackend, HostUsbError, UsbControlSetup};
use crate::mem::{read_trb, write_bytes};
use crate::trb::{TRB_TYPE_DATA_STAGE, TRB_TYPE_STATUS_STAGE};
use crate::xhci::Xhci;

/// One host device bound to a root port.
pub struct PassthroughBinding {
    /// Guest root port (1-based).
    pub port: u8,
    /// Host backend (claimed device).
    pub backend: Box<dyn HostUsbBackend>,
}

/// Parameters for a guest control transfer forwarded to the host.
pub struct ControlJob {
    /// Guest root port.
    pub port: u8,
    /// xHCI slot id.
    pub slot_id: u8,
    /// Setup packet.
    pub setup: UsbControlSetup,
    /// OUT data stage (if any).
    pub data_out: Vec<u8>,
    /// IN buffer capacity.
    pub data_in_len: usize,
    /// Setup TRB guest address (start of the control transfer block).
    pub setup_trb_ptr: u64,
}

/// Bulk / interrupt transfer forwarded to the host.
pub struct PipeJob {
    /// Guest root port.
    pub port: u8,
    /// xHCI slot id.
    pub slot_id: u8,
    /// Doorbell target (DCI).
    pub dci: u8,
    /// USB `bEndpointAddress`.
    pub endpoint_address: u8,
    /// TRB pointer to complete.
    pub trb_ptr: u64,
    /// Guest buffer GPA.
    pub data_gpa: u64,
    /// Max length from the TRB.
    pub length: usize,
    /// `true` for IN (device-to-host).
    pub is_in: bool,
}

/// Pending work for the passthrough worker.
enum Job {
    Control(ControlJob),
    Pipe(PipeJob),
    Shutdown,
}

/// Shared state the worker uses to post transfer events.
struct WorkerState {
    memory: Arc<Mutex<dyn GuestMemoryAccess + Send>>,
    xhci: Weak<Mutex<Xhci>>,
}

/// Handle to enqueue passthrough jobs and stop the worker at VM teardown.
pub struct PassthroughWorker {
    tx: Sender<Job>,
    thread: Option<JoinHandle<()>>,
}

impl std::fmt::Debug for PassthroughWorker {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PassthroughWorker")
            .field("running", &self.thread.is_some())
            .finish()
    }
}

impl PassthroughWorker {
    /// Start the worker thread.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all, fields(bindings = bindings.len()))]
    pub fn start(
        mut bindings: Vec<PassthroughBinding>,
        memory: Arc<Mutex<dyn GuestMemoryAccess + Send>>,
        xhci: Arc<Mutex<Xhci>>,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let (disc_tx, disc_rx) = mpsc::channel();
        for binding in &mut bindings {
            binding
                .backend
                .wire_disconnect_notifier(binding.port, disc_tx.clone());
        }
        let backends: Vec<_> = bindings.into_iter().map(|b| (b.port, b.backend)).collect();
        let backend_count = backends.len();
        let state = WorkerState {
            memory,
            xhci: Arc::downgrade(&xhci),
        };
        let thread = thread::Builder::new()
            .name("ternvale-usb-passthrough".into())
            .spawn(move || run_worker(rx, disc_rx, backends, state))
            .expect("spawn usb passthrough worker");
        tracing::info!(
            target: "ternvale::usb",
            backends = backend_count,
            "usb passthrough worker started"
        );
        Self {
            tx,
            thread: Some(thread),
        }
    }

    /// Queue a control transfer already parsed from the guest setup TRB.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip(self, job))]
    pub fn submit_control(&self, job: ControlJob) {
        let slot_id = job.slot_id;
        if let Err(err) = self.tx.send(Job::Control(job)) {
            tracing::warn!(
                target: "ternvale::usb",
                slot_id,
                error = %err,
                "passthrough worker queue full or stopped"
            );
        }
    }

    /// Queue a bulk or interrupt transfer.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip(self, job))]
    pub fn submit_pipe(&self, job: PipeJob) {
        let slot_id = job.slot_id;
        if let Err(err) = self.tx.send(Job::Pipe(job)) {
            tracing::warn!(
                target: "ternvale::usb",
                slot_id,
                error = %err,
                "passthrough pipe queue full or stopped"
            );
        }
    }

    /// Stop the worker and join the thread.
    #[tracing::instrument(level = "debug", target = "ternvale::usb", skip_all)]
    pub fn shutdown(&mut self) {
        let _ = self.tx.send(Job::Shutdown);
        if let Some(handle) = self.thread.take() {
            if let Err(err) = handle.join() {
                tracing::warn!(target: "ternvale::usb", error = ?err, "passthrough worker join failed");
            }
        }
        tracing::info!(target: "ternvale::usb", "usb passthrough worker stopped");
    }
}

fn run_worker(
    rx: Receiver<Job>,
    disc_rx: Receiver<u8>,
    backends: Vec<(u8, Box<dyn HostUsbBackend>)>,
    state: WorkerState,
) {
    loop {
        while let Ok(port) = disc_rx.try_recv() {
            handle_host_disconnect(&backends, port, &state);
        }
        match rx.recv_timeout(Duration::from_millis(250)) {
            Ok(Job::Shutdown) => break,
            Ok(Job::Control(job)) => {
                let slot_id = job.slot_id;
                if let Err(err) = execute_control(&backends, job, &state) {
                    tracing::warn!(
                        target: "ternvale::usb",
                        slot_id,
                        error = %err,
                        "passthrough control completion failed"
                    );
                }
            }
            Ok(Job::Pipe(job)) => {
                let slot_id = job.slot_id;
                if let Err(err) = execute_pipe(&backends, job, &state) {
                    tracing::warn!(
                        target: "ternvale::usb",
                        slot_id,
                        error = %err,
                        "passthrough pipe completion failed"
                    );
                }
            }
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
}

fn handle_host_disconnect(
    backends: &[(u8, Box<dyn HostUsbBackend>)],
    port: u8,
    state: &WorkerState,
) {
    if let Some((_, backend)) = backends.iter().find(|(p, _)| *p == port) {
        backend.on_host_disconnect();
    }
    let mut mem = match state.memory.lock() {
        Ok(guard) => guard,
        Err(_) => {
            tracing::warn!(target: "ternvale::usb", port, "guest memory lock poisoned on usb disconnect");
            return;
        }
    };
    let Some(xhci) = state.xhci.upgrade() else {
        tracing::warn!(target: "ternvale::usb", port, "xhci dropped on host usb disconnect");
        return;
    };
    let mut guard = match xhci.lock() {
        Ok(g) => g,
        Err(_) => {
            tracing::warn!(target: "ternvale::usb", port, "xhci lock poisoned on usb disconnect");
            return;
        }
    };
    guard.handle_host_usb_disconnect(port, &mut *mem);
}

fn execute_control(
    backends: &[(u8, Box<dyn HostUsbBackend>)],
    job: ControlJob,
    state: &WorkerState,
) -> Result<(), MemError> {
    let backend = backends
        .iter()
        .find(|(p, _)| *p == job.port)
        .map(|(_, b)| b.as_ref())
        .ok_or_else(|| MemError::Read {
            gpa: 0,
            message: format!("no host backend for port {}", job.port),
        })?;
    let mut in_buf = vec![0u8; job.data_in_len.min(4096)];
    let out_slice = if job.data_out.is_empty() {
        None
    } else {
        Some(job.data_out.as_slice())
    };
    let (code, bytes) = match backend.control_transfer(job.setup, out_slice, &mut in_buf) {
        Ok(n) => {
            tracing::debug!(
                target: "ternvale::usb",
                slot_id = job.slot_id,
                bytes = n,
                "host control transfer ok"
            );
            (CC_SUCCESS, n as u32)
        }
        Err(HostUsbError::Unavailable { message }) => {
            tracing::warn!(
                target: "ternvale::usb",
                slot_id = job.slot_id,
                message,
                "host device unavailable"
            );
            (CC_STALL_ERROR, 0)
        }
        Err(err) => {
            tracing::warn!(
                target: "ternvale::usb",
                slot_id = job.slot_id,
                error = %err,
                "host control transfer failed"
            );
            (CC_STALL_ERROR, 0)
        }
    };
    let mut mem = state.memory.lock().map_err(|_| MemError::Write {
        gpa: 0,
        message: "guest memory lock poisoned".into(),
    })?;
    let Some(xhci) = state.xhci.upgrade() else {
        tracing::warn!(target: "ternvale::usb", "xhci dropped before passthrough completion");
        return Ok(());
    };
    let mut guard = xhci.lock().map_err(|_| MemError::Write {
        gpa: 0,
        message: "xhci lock poisoned".into(),
    })?;
    finish_ep0_control(&mut guard, &job, code, bytes, &in_buf, &mut *mem)?;
    guard.pulse_irq();
    Ok(())
}

fn finish_ep0_control(
    xhci: &mut Xhci,
    job: &ControlJob,
    code: u32,
    bytes: u32,
    in_buf: &[u8],
    mem: &mut dyn GuestMemoryAccess,
) -> Result<(), MemError> {
    let slot_id = job.slot_id;
    let setup_ptr = job.setup_trb_ptr;
    let is_in = job.setup.request_type & 0x80 != 0;
    xhci.post_passthrough_completion(
        slot_id,
        0,
        code,
        setup_ptr,
        if code == CC_SUCCESS { 8 } else { 0 },
        mem,
    )?;
    if code != CC_SUCCESS {
        if let Some(slot) = xhci.slots_mut().slot_mut(slot_id) {
            slot.ep0_passthrough_busy = false;
        }
        return Ok(());
    }
    let mut ptr = setup_ptr + 16;
    if is_in && bytes > 0 {
        let data_trb = read_trb(mem, ptr)?;
        if data_trb.trb_type() == TRB_TYPE_DATA_STAGE {
            let len = bytes.min(data_trb.status as u32) as usize;
            write_bytes(mem, data_trb.param, &in_buf[..len.min(in_buf.len())])?;
            xhci.post_passthrough_completion(slot_id, 0, CC_SUCCESS, ptr, len as u32, mem)?;
            ptr += 16;
        }
    } else if !is_in && job.setup.length > 0 {
        ptr += 16;
    }
    let status_trb = read_trb(mem, ptr)?;
    if status_trb.trb_type() == TRB_TYPE_STATUS_STAGE {
        xhci.post_passthrough_completion(slot_id, 0, CC_SUCCESS, ptr, 0, mem)?;
        ptr += 16;
    }
    if let Some(slot) = xhci.slots_mut().slot_mut(slot_id) {
        slot.ep0.dequeue = ptr;
        slot.ep0_passthrough_busy = false;
    }
    Ok(())
}

fn execute_pipe(
    backends: &[(u8, Box<dyn HostUsbBackend>)],
    job: PipeJob,
    state: &WorkerState,
) -> Result<(), MemError> {
    let backend = backends
        .iter()
        .find(|(p, _)| *p == job.port)
        .map(|(_, b)| b.as_ref())
        .ok_or_else(|| MemError::Read {
            gpa: 0,
            message: format!("no host backend for port {}", job.port),
        })?;
    let mut in_buf = vec![0u8; job.length.min(65536)];
    let mut out_buf = Vec::new();
    if !job.is_in {
        let mem = state.memory.lock().map_err(|_| MemError::Read {
            gpa: job.data_gpa,
            message: "guest memory lock poisoned".into(),
        })?;
        out_buf = crate::mem::read_bytes(&*mem, job.data_gpa, job.length)?;
    }
    let (code, bytes) = if job.is_in {
        match backend.pipe_transfer(job.endpoint_address, None, &mut in_buf) {
            Ok(n) => (CC_SUCCESS, n as u32),
            Err(HostUsbError::Unavailable { message }) => {
                tracing::warn!(
                    target: "ternvale::usb",
                    slot_id = job.slot_id,
                    message,
                    "host pipe unavailable"
                );
                (CC_STALL_ERROR, 0)
            }
            Err(err) => {
                tracing::warn!(
                    target: "ternvale::usb",
                    slot_id = job.slot_id,
                    error = %err,
                    "host pipe transfer failed"
                );
                (CC_STALL_ERROR, 0)
            }
        }
    } else {
        match backend.pipe_transfer(job.endpoint_address, Some(&out_buf), &mut []) {
            Ok(n) => (CC_SUCCESS, n as u32),
            Err(HostUsbError::Unavailable { message }) => {
                tracing::warn!(
                    target: "ternvale::usb",
                    slot_id = job.slot_id,
                    message,
                    "host pipe unavailable"
                );
                (CC_STALL_ERROR, 0)
            }
            Err(err) => {
                tracing::warn!(
                    target: "ternvale::usb",
                    slot_id = job.slot_id,
                    error = %err,
                    "host pipe transfer failed"
                );
                (CC_STALL_ERROR, 0)
            }
        }
    };
    let mut mem = state.memory.lock().map_err(|_| MemError::Write {
        gpa: job.data_gpa,
        message: "guest memory lock poisoned".into(),
    })?;
    if job.is_in && bytes > 0 {
        write_bytes(&mut *mem, job.data_gpa, &in_buf[..bytes as usize])?;
    }
    let Some(xhci) = state.xhci.upgrade() else {
        return Ok(());
    };
    let mut guard = xhci.lock().map_err(|_| MemError::Write {
        gpa: 0,
        message: "xhci lock poisoned".into(),
    })?;
    guard.post_passthrough_completion(job.slot_id, job.dci, code, job.trb_ptr, bytes, &mut *mem)?;
    if let Some(slot) = guard.slots_mut().slot_mut(job.slot_id) {
        let ring = slot.ep_ring_mut(job.dci);
        ring.dequeue = job.trb_ptr + 16;
        ring.passthrough_busy = false;
    }
    guard.pulse_irq();
    Ok(())
}
