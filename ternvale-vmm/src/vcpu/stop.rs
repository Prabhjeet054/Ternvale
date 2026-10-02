//! [`VcpuStop`]: cancel a vCPU from a thread that does not own it.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar};

use super::VcpuError;

/// Sends `hv_vcpus_exit` for one vCPU. This value is `Send`.
#[derive(Debug, Clone)]
pub struct VcpuStop {
    pub(super) id: u64,
    pub(super) stop: Arc<AtomicBool>,
    pub(super) pending: Arc<AtomicBool>,
    pub(super) wake: Arc<Condvar>,
}

impl VcpuStop {
    /// Mark the vCPU stopped and ask the kernel to cancel it.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn request(&self) -> Result<(), VcpuError> {
        self.stop.store(true, Ordering::Release);
        self.pending.store(true, Ordering::Release);
        self.wake.notify_one();
        tracing::info!(target: "ternvale::vcpu", vcpu_id = self.id, "stop requested");
        ternvale_hv::vcpus_exit(&[self.id])?;
        Ok(())
    }

    /// Cancel `hv_vcpu_run` without stopping the guest.
    ///
    /// The machine loop uses this to leave a hypervisor WFI and drain stdin.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn nudge(&self) -> Result<(), VcpuError> {
        self.pending.store(true, Ordering::Release);
        self.wake.notify_one();
        tracing::debug!(target: "ternvale::vcpu", vcpu_id = self.id, "vcpu nudge");
        ternvale_hv::vcpus_exit(&[self.id])?;
        Ok(())
    }

    /// [`VcpuStop::nudge`] every vCPU in `stops` with one `hv_vcpus_exit` call.
    /// The guest keeps running; pause uses this to get every vCPU back to the
    /// host loop.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(count = stops.len()))]
    pub fn nudge_all(stops: &[VcpuStop]) -> Result<(), VcpuError> {
        if stops.is_empty() {
            return Ok(());
        }
        let ids: Vec<u64> = stops.iter().map(|stop| stop.id).collect();
        for stop in stops {
            stop.pending.store(true, Ordering::Release);
            stop.wake.notify_one();
        }
        tracing::debug!(target: "ternvale::vcpu", vcpu_ids = ?ids, "nudging every vcpu");
        ternvale_hv::vcpus_exit(&ids)?;
        Ok(())
    }

    /// Whether [`VcpuStop::request`] has run.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }

    /// A handle with no vCPU behind it, for tests that never call the kernel.
    #[cfg(test)]
    pub(crate) fn detached(id: u64) -> Self {
        Self {
            id,
            stop: Arc::new(AtomicBool::new(false)),
            pending: Arc::new(AtomicBool::new(false)),
            wake: Arc::new(Condvar::new()),
        }
    }

    /// Kernel id of the vCPU this handle cancels.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vcpu_id = self.id))]
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Stop every vCPU in `stops` with one `hv_vcpus_exit` call.
    ///
    /// Each stop flag is set and each WFI park is woken first, so a vCPU that
    /// is between runs also sees the request.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(count = stops.len()))]
    pub fn request_all(stops: &[VcpuStop]) -> Result<(), VcpuError> {
        if stops.is_empty() {
            return Ok(());
        }
        let ids: Vec<u64> = stops.iter().map(|stop| stop.id).collect();
        for stop in stops {
            stop.stop.store(true, Ordering::Release);
            stop.pending.store(true, Ordering::Release);
            stop.wake.notify_one();
        }
        tracing::info!(target: "ternvale::vcpu", vcpu_ids = ?ids, "stop requested on all vcpus");
        ternvale_hv::vcpus_exit(&ids)?;
        Ok(())
    }
}
