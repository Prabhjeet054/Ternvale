//! Full-VM snapshot capture/restore while the guest is paused.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use ternvale_hv::SysReg;
use ternvale_snapshot::{new_meta, read_bundle, write_bundle, SnapshotError, VmSnapshotPayload};

use super::{ControlError, VmControl, VmState, KICK_EVERY};
use crate::memory::GuestMemory;
use crate::vcpu::{Vcpu, VcpuError};

const VCPU_WAIT: Duration = Duration::from_secs(10);

/// EL1 / timer registers saved with each vCPU.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VcpuGuestState {
    pub cpu: u32,
    pub gprs: [u64; 31],
    pub sp_el1: u64,
    pub pc: u64,
    pub cpsr: u64,
    pub vtimer_offset: u64,
    pub sctlr_el1: u64,
    pub ttbr0_el1: u64,
    pub ttbr1_el1: u64,
    pub tcr_el1: u64,
    pub mair_el1: u64,
    pub vbar_el1: u64,
    pub spsr_el1: u64,
    pub elr_el1: u64,
    pub esr_el1: u64,
    pub far_el1: u64,
    pub mpidr_el1: u64,
    pub cntv_ctl_el0: u64,
    pub cntv_cval_el0: u64,
}

/// Capture guest registers on the vCPU's owning thread.
#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(cpu = index))]
pub fn capture_vcpu(index: u32, vcpu: &Vcpu) -> Result<VcpuGuestState, VcpuError> {
    let mut gprs = [0u64; 31];
    for n in 0..31u8 {
        gprs[n as usize] = vcpu.get_x(n)?;
    }
    Ok(VcpuGuestState {
        cpu: index,
        gprs,
        sp_el1: vcpu.get_sp_el1()?,
        pc: vcpu.get_pc()?,
        cpsr: vcpu.get_cpsr()?,
        vtimer_offset: vcpu.vtimer_offset()?,
        sctlr_el1: vcpu.get_sys_reg(SysReg::SctlrEl1)?,
        ttbr0_el1: vcpu.get_sys_reg(SysReg::Ttbr0El1)?,
        ttbr1_el1: vcpu.get_sys_reg(SysReg::Ttbr1El1)?,
        tcr_el1: vcpu.get_sys_reg(SysReg::TcrEl1)?,
        mair_el1: vcpu.get_sys_reg(SysReg::MairEl1)?,
        vbar_el1: vcpu.get_sys_reg(SysReg::VbarEl1)?,
        spsr_el1: vcpu.get_sys_reg(SysReg::SpsrEl1)?,
        elr_el1: vcpu.get_sys_reg(SysReg::ElrEl1)?,
        esr_el1: vcpu.get_sys_reg(SysReg::EsrEl1)?,
        far_el1: vcpu.get_sys_reg(SysReg::FarEl1)?,
        mpidr_el1: vcpu.get_sys_reg(SysReg::MpidrEl1)?,
        cntv_ctl_el0: vcpu.get_sys_reg(SysReg::CntvCtlEl0)?,
        cntv_cval_el0: vcpu.get_sys_reg(SysReg::CntvCvalEl0)?,
    })
}

#[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(cpu = state.cpu))]
pub fn restore_vcpu(vcpu: &Vcpu, state: &VcpuGuestState) -> Result<(), VcpuError> {
    for (index, value) in state.gprs.iter().enumerate() {
        vcpu.set_x(index as u8, *value)?;
    }
    vcpu.set_sp_el1(state.sp_el1)?;
    vcpu.set_pc(state.pc)?;
    vcpu.set_cpsr(state.cpsr)?;
    vcpu.set_vtimer_offset(state.vtimer_offset)?;
    vcpu.set_sys_reg(SysReg::SctlrEl1, state.sctlr_el1)?;
    vcpu.set_sys_reg(SysReg::Ttbr0El1, state.ttbr0_el1)?;
    vcpu.set_sys_reg(SysReg::Ttbr1El1, state.ttbr1_el1)?;
    vcpu.set_sys_reg(SysReg::TcrEl1, state.tcr_el1)?;
    vcpu.set_sys_reg(SysReg::MairEl1, state.mair_el1)?;
    vcpu.set_sys_reg(SysReg::VbarEl1, state.vbar_el1)?;
    vcpu.set_sys_reg(SysReg::SpsrEl1, state.spsr_el1)?;
    vcpu.set_sys_reg(SysReg::ElrEl1, state.elr_el1)?;
    vcpu.set_sys_reg(SysReg::EsrEl1, state.esr_el1)?;
    vcpu.set_sys_reg(SysReg::FarEl1, state.far_el1)?;
    vcpu.set_sys_reg(SysReg::MpidrEl1, state.mpidr_el1)?;
    vcpu.set_sys_reg(SysReg::CntvCtlEl0, state.cntv_ctl_el0)?;
    vcpu.set_sys_reg(SysReg::CntvCvalEl0, state.cntv_cval_el0)?;
    Ok(())
}

/// RAM + GIC resources shared with the control socket.
pub struct SnapshotHost {
    pub memory: Arc<Mutex<GuestMemory>>,
    pub gic: Arc<ternvale_hv::Gic>,
    pub ram_base: u64,
    pub ram_size: u64,
    pub vm_name: String,
    pub device_note: String,
}

impl SnapshotHost {
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(vm = %self.vm_name, dir = %dir.display()))]
    pub fn write_bundle(
        &self,
        dir: &Path,
        label: &str,
        vcpus: &[VcpuGuestState],
    ) -> Result<(), SnapshotError> {
        let gic = self
            .gic
            .capture_state()
            .map_err(|error| SnapshotError::InvalidRequest {
                detail: format!("gic capture: {error}"),
            })?;
        let ram = {
            let mem = crate::lockwatch::lock(&self.memory, "guest-memory");
            mem.copy_region(self.ram_base, self.ram_size)
                .map_err(|error| SnapshotError::InvalidRequest {
                    detail: format!("ram capture: {error}"),
                })?
        };
        let meta = new_meta(
            &self.vm_name,
            label,
            self.ram_base,
            self.ram_size,
            gic.len(),
            vcpus.len() as u32,
        );
        let vcpus_json =
            serde_json::to_string_pretty(vcpus).map_err(|error| SnapshotError::InvalidRequest {
                detail: format!("serialize vcpus: {error}"),
            })?;
        let devices_json = serde_json::json!({
            "note": self.device_note,
            "virtio_queues": "not captured in v1",
        })
        .to_string();
        write_bundle(
            dir,
            &VmSnapshotPayload {
                meta,
                gic,
                ram,
                vcpus_json,
                devices_json,
            },
        )
    }

    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(dir = %dir.display()))]
    pub fn read_bundle(
        &self,
        dir: &Path,
    ) -> Result<(VmSnapshotPayload, Vec<VcpuGuestState>), SnapshotError> {
        let payload = read_bundle(dir)?;
        let vcpus: Vec<VcpuGuestState> =
            serde_json::from_str(&payload.vcpus_json).map_err(|error| {
                SnapshotError::InvalidManifest {
                    path: dir.join("vcpus.json"),
                    detail: format!("parse vcpus: {error}"),
                }
            })?;
        Ok((payload, vcpus))
    }

    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all)]
    pub fn apply_bundle(
        &self,
        payload: &VmSnapshotPayload,
        vcpus: &[VcpuGuestState],
    ) -> Result<(), SnapshotError> {
        if payload.ram.len() as u64 != self.ram_size {
            return Err(SnapshotError::InvalidRequest {
                detail: format!(
                    "ram size mismatch: bundle {} bytes, vm {} bytes",
                    payload.ram.len(),
                    self.ram_size
                ),
            });
        }
        {
            let mut mem = crate::lockwatch::lock(&self.memory, "guest-memory");
            mem.write_bytes(self.ram_base, &payload.ram)
                .map_err(|error| SnapshotError::InvalidRequest {
                    detail: format!("restore ram: {error}"),
                })?;
        }
        self.gic
            .restore_state(&payload.gic)
            .map_err(|error| SnapshotError::InvalidRequest {
                detail: format!("gic restore: {error}"),
            })?;
        if vcpus.len() != payload.meta.vcpu_count as usize {
            return Err(SnapshotError::InvalidRequest {
                detail: "vcpu count in meta does not match vcpus.json".to_string(),
            });
        }
        Ok(())
    }
}

enum SessionKind {
    Capture,
    Restore(Vec<VcpuGuestState>),
}

struct Inner {
    kind: Option<SessionKind>,
    pending: u32,
    states: Vec<Option<VcpuGuestState>>,
    error: Option<String>,
}

/// Coordinates per-vCPU work on vCPU threads.
pub struct Session {
    inner: Mutex<Inner>,
    changed: Condvar,
}

impl Session {
    pub fn idle() -> Self {
        Self {
            inner: Mutex::new(Inner {
                kind: None,
                pending: 0,
                states: Vec::new(),
                error: None,
            }),
            changed: Condvar::new(),
        }
    }

    pub fn vcpu_tick(&self, cpu: u32, capture: impl FnOnce() -> Result<VcpuGuestState, String>) {
        let mut guard = crate::lockwatch::lock(&self.inner, "snapshot-session");
        if !matches!(guard.kind, Some(SessionKind::Capture)) || guard.error.is_some() {
            return;
        }
        let bit = 1u32 << cpu;
        if guard.pending & bit == 0 {
            return;
        }
        match capture() {
            Ok(state) => {
                if let Some(slot) = guard.states.get_mut(cpu as usize) {
                    *slot = Some(state);
                }
                guard.pending &= !bit;
                tracing::debug!(target: "ternvale::snapshot", cpu, pending = guard.pending, "vcpu snapshot captured");
            }
            Err(detail) => {
                guard.error = Some(detail);
                guard.pending = 0;
            }
        }
        self.changed.notify_all();
    }

    pub fn restore_tick(
        &self,
        cpu: u32,
        apply: impl FnOnce(&VcpuGuestState) -> Result<(), String>,
    ) {
        let mut guard = crate::lockwatch::lock(&self.inner, "snapshot-session");
        let states = match &guard.kind {
            Some(SessionKind::Restore(states)) => states,
            _ => return,
        };
        if guard.error.is_some() {
            return;
        }
        let bit = 1u32 << cpu;
        if guard.pending & bit == 0 {
            return;
        }
        let Some(state) = states.get(cpu as usize) else {
            return;
        };
        if let Err(detail) = apply(state) {
            guard.error = Some(detail);
            guard.pending = 0;
        } else {
            guard.pending &= !bit;
            tracing::debug!(target: "ternvale::snapshot", cpu, pending = guard.pending, "vcpu snapshot restored");
        }
        self.changed.notify_all();
    }

    fn begin_capture(&self, cpus: u32) -> Result<(), String> {
        let mut guard = crate::lockwatch::lock(&self.inner, "snapshot-session");
        if guard.kind.is_some() {
            return Err("snapshot session already active".to_string());
        }
        guard.kind = Some(SessionKind::Capture);
        guard.pending = if cpus == 0 { 0 } else { (1u32 << cpus) - 1 };
        guard.states = vec![None; cpus as usize];
        guard.error = None;
        self.changed.notify_all();
        Ok(())
    }

    fn begin_restore(&self, states: Vec<VcpuGuestState>) -> Result<(), String> {
        let mut guard = crate::lockwatch::lock(&self.inner, "snapshot-session");
        if guard.kind.is_some() {
            return Err("snapshot session already active".to_string());
        }
        let cpus = states.len() as u32;
        guard.kind = Some(SessionKind::Restore(states));
        guard.pending = if cpus == 0 { 0 } else { (1u32 << cpus) - 1 };
        guard.states = vec![None; cpus as usize];
        guard.error = None;
        self.changed.notify_all();
        Ok(())
    }

    fn wait_capture(
        &self,
        control: &VmControl,
        timeout: Duration,
    ) -> Result<Vec<VcpuGuestState>, String> {
        let started = Instant::now();
        let mut guard = crate::lockwatch::lock(&self.inner, "snapshot-session");
        loop {
            if let Some(error) = guard.error.clone() {
                guard.kind = None;
                return Err(error);
            }
            if guard.pending == 0 {
                let states = guard
                    .states
                    .iter()
                    .map(|slot| slot.clone().ok_or_else(|| "missing vcpu state".to_string()))
                    .collect::<Result<Vec<_>, _>>()?;
                guard.kind = None;
                return Ok(states);
            }
            if started.elapsed() >= timeout {
                guard.kind = None;
                return Err(format!(
                    "timed out waiting for vcpu capture (pending {:#x})",
                    guard.pending
                ));
            }
            guard = self.wait(guard);
            drop(guard);
            control.call_kick();
            guard = crate::lockwatch::lock(&self.inner, "snapshot-session");
        }
    }

    fn wait_restore(&self, control: &VmControl, timeout: Duration) -> Result<(), String> {
        let started = Instant::now();
        let mut guard = crate::lockwatch::lock(&self.inner, "snapshot-session");
        loop {
            if let Some(error) = guard.error.clone() {
                guard.kind = None;
                return Err(error);
            }
            if guard.pending == 0 {
                guard.kind = None;
                return Ok(());
            }
            if started.elapsed() >= timeout {
                guard.kind = None;
                return Err(format!(
                    "timed out waiting for vcpu restore (pending {:#x})",
                    guard.pending
                ));
            }
            guard = self.wait(guard);
            drop(guard);
            control.call_kick();
            guard = crate::lockwatch::lock(&self.inner, "snapshot-session");
        }
    }

    fn wait<'a>(
        &self,
        guard: crate::lockwatch::Guard<'a, Inner>,
    ) -> crate::lockwatch::Guard<'a, Inner> {
        let (guard, _) = guard.wait_timeout(&self.changed, KICK_EVERY);
        guard
    }
}

impl VmControl {
    /// Pause, capture RAM/GIC/vCPU state, write `dir`, resume.
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(vm = %self.name, label, dir = %dir.display()))]
    pub fn snapshot_create(&self, label: &str, dir: &Path) -> Result<PathBuf, ControlError> {
        let host = self
            .snapshot_host
            .get()
            .cloned()
            .ok_or(ControlError::SnapshotHostMissing)?;
        let ram_bytes = host.ram_size;
        let ram_mib = ram_bytes / (1024 * 1024);
        let started = Instant::now();
        self.pause(Duration::from_secs(30))?;
        let pause_ms = millis(started.elapsed());
        let result = self.snapshot_create_paused(&host, label, dir);
        let capture_ms = millis(started.elapsed()) - pause_ms;
        let _ = self.resume();
        let total_ms = millis(started.elapsed());
        match &result {
            Ok(path) => tracing::info!(
                target: "ternvale::snapshot",
                vm = %self.name,
                label,
                path = %path.display(),
                ram_mib,
                ram_bytes,
                pause_ms,
                capture_ms,
                create_ms = total_ms,
                "vm snapshot create completed"
            ),
            Err(error) => tracing::warn!(
                target: "ternvale::snapshot",
                vm = %self.name,
                label,
                ram_mib,
                error = %error,
                create_ms = total_ms,
                "vm snapshot create failed"
            ),
        }
        result
    }

    /// Pause, restore from `dir`, resume.
    #[tracing::instrument(level = "debug", target = "ternvale::snapshot", skip_all, fields(vm = %self.name, dir = %dir.display()))]
    pub fn snapshot_restore(&self, dir: &Path) -> Result<(), ControlError> {
        let host = self
            .snapshot_host
            .get()
            .cloned()
            .ok_or(ControlError::SnapshotHostMissing)?;
        let ram_bytes = host.ram_size;
        let ram_mib = ram_bytes / (1024 * 1024);
        let started = Instant::now();
        self.pause(Duration::from_secs(30))?;
        let pause_ms = millis(started.elapsed());
        let result = self.snapshot_restore_paused(&host, dir);
        let apply_ms = millis(started.elapsed()) - pause_ms;
        let _ = self.resume();
        let total_ms = millis(started.elapsed());
        match &result {
            Ok(()) => tracing::info!(
                target: "ternvale::snapshot",
                vm = %self.name,
                path = %dir.display(),
                ram_mib,
                ram_bytes,
                pause_ms,
                apply_ms,
                restore_ms = total_ms,
                "vm snapshot restore completed"
            ),
            Err(error) => tracing::warn!(
                target: "ternvale::snapshot",
                vm = %self.name,
                path = %dir.display(),
                ram_mib,
                error = %error,
                restore_ms = total_ms,
                "vm snapshot restore failed"
            ),
        }
        result
    }

    fn snapshot_create_paused(
        &self,
        host: &SnapshotHost,
        label: &str,
        dir: &Path,
    ) -> Result<PathBuf, ControlError> {
        if self.state() != VmState::Paused {
            return Err(self.reject("snapshot create", self.state()));
        }
        self.snapshot
            .begin_capture(self.cpus())
            .map_err(map_snap_str)?;
        self.changed.notify_all();
        let vcpus = self
            .snapshot
            .wait_capture(self, VCPU_WAIT)
            .map_err(map_snap_str)?;
        host.write_bundle(dir, label, &vcpus).map_err(map_snap)?;
        tracing::info!(
            target: "ternvale::snapshot",
            vm = %self.name,
            label,
            path = %dir.display(),
            "vm snapshot created"
        );
        Ok(dir.to_path_buf())
    }

    fn snapshot_restore_paused(&self, host: &SnapshotHost, dir: &Path) -> Result<(), ControlError> {
        if self.state() != VmState::Paused {
            return Err(self.reject("snapshot restore", self.state()));
        }
        let (payload, vcpus) = host.read_bundle(dir).map_err(map_snap)?;
        host.apply_bundle(&payload, &vcpus).map_err(map_snap)?;
        self.snapshot.begin_restore(vcpus).map_err(map_snap_str)?;
        self.changed.notify_all();
        self.snapshot
            .wait_restore(self, VCPU_WAIT)
            .map_err(map_snap_str)?;
        tracing::info!(
            target: "ternvale::snapshot",
            vm = %self.name,
            path = %dir.display(),
            "vm snapshot restored"
        );
        Ok(())
    }

    /// vCPU thread hook for restore.
    pub(crate) fn snapshot_restore_tick(
        &self,
        cpu: u32,
        apply: impl FnOnce(&VcpuGuestState) -> Result<(), String>,
    ) {
        self.snapshot.restore_tick(cpu, apply);
    }
}

fn map_snap_str(detail: String) -> ControlError {
    ControlError::Snapshot { detail }
}

fn map_snap(error: SnapshotError) -> ControlError {
    ControlError::Snapshot {
        detail: error.to_string(),
    }
}

fn millis(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
