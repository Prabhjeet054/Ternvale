//! SMP power state shared by every vCPU thread.
//!
//! CPU `n` has MPIDR affinity `Aff1 = n / 16`, `Aff0 = n % 16`, the layout QEMU
//! virt uses with GICv3 (16 is the width of the `ICC_SGI1R_EL1` target list).
//! CPU 0 starts powered on. The others wait in [`CpuPower::wait_for_on`] until
//! the guest calls PSCI `CPU_ON`. Any vCPU can end the VM through
//! [`CpuPower::request_stop`], which cancels every registered vCPU with one
//! `hv_vcpus_exit` and wakes every thread waiting here.
//!
//! The table mutex is a leaf lock: nothing else is locked while it is held,
//! and the only call made under it is `hv_vcpus_exit`.

use std::sync::{Condvar, Mutex, MutexGuard};

use crate::psci::{
    sign, PowerRequest, AFFINITY_OFF, AFFINITY_ON, AFFINITY_ON_PENDING, ALREADY_ON,
    INVALID_ADDRESS, INVALID_PARAMS, ON_PENDING, SUCCESS,
};
use crate::vcpu::{ExitReason, VcpuStop};

/// `MPIDR_EL1` bit 31 is RES1.
pub const MPIDR_RES1: u64 = 1 << 31;
/// Aff3 (bits 39:32) and Aff2..Aff0 (bits 23:0). Linux's `MPIDR_HWID_BITMASK`.
pub const MPIDR_AFFINITY_MASK: u64 = 0xff_00ff_ffff;
/// CPUs per Aff1 cluster.
const CLUSTER: u32 = 16;

/// `MPIDR_EL1` the host writes for CPU `index`.
#[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(cpu = index))]
pub fn mpidr(index: u32) -> u64 {
    MPIDR_RES1 | (u64::from(index / CLUSTER) << 8) | u64::from(index % CLUSTER)
}

/// Device-tree `/cpus/cpu@N` `reg` (one cell: Aff2..Aff0) for CPU `index`.
#[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(cpu = index))]
pub fn dt_cpu_reg(index: u32) -> u32 {
    (mpidr(index) & 0x00ff_ffff) as u32
}

/// `GICR_TYPER.Affinity_Value` (bits 63:32): Aff3.Aff2.Aff1.Aff0.
#[tracing::instrument(level = "debug", target = "ternvale::gic", skip_all)]
pub fn gicr_affinity(mpidr: u64) -> u64 {
    (((mpidr >> 32) & 0xff) << 24) | (mpidr & 0x00ff_ffff)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CpuState {
    Off,
    OnPending { entry: u64, context: u64 },
    On,
}

struct Table {
    cpus: Vec<CpuState>,
    stops: Vec<Option<VcpuStop>>,
    ready: Vec<bool>,
    /// CPUs that have called `hv_vcpu_create`, in index order.
    created: u32,
    stopping: Option<ExitReason>,
}

/// Power state of every guest CPU.
pub struct CpuPower {
    ram_base: u64,
    ram_end: u64,
    table: Mutex<Table>,
    changed: Condvar,
}

impl CpuPower {
    /// `count` CPUs; CPU 0 is on. `CPU_ON` entry points must be inside RAM.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(count))]
    pub fn new(count: u32, ram_base: u64, ram_size: u64) -> Self {
        let mut cpus = vec![CpuState::Off; count as usize];
        if let Some(boot) = cpus.first_mut() {
            *boot = CpuState::On;
        }
        tracing::debug!(target: "ternvale::psci", count, "cpu power table created");
        Self {
            ram_base,
            ram_end: ram_base.saturating_add(ram_size),
            table: Mutex::new(Table {
                cpus,
                stops: vec![None; count as usize],
                ready: vec![false; count as usize],
                created: 0,
                stopping: None,
            }),
            changed: Condvar::new(),
        }
    }

    /// Number of CPUs.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all)]
    pub fn count(&self) -> u32 {
        self.lock().cpus.len() as u32
    }

    /// CPU index whose MPIDR affinity is `target`.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(target = format!("{target:#x}")))]
    pub fn index_of(&self, target: u64) -> Option<u32> {
        if target & !MPIDR_AFFINITY_MASK != 0 {
            return None;
        }
        (0..self.count()).find(|&index| mpidr(index) & MPIDR_AFFINITY_MASK == target)
    }

    /// Block until CPUs `0..index` have created their vCPUs. The framework
    /// hands out vCPU ids (and redistributor frames) in creation order, so
    /// this keeps vCPU id == CPU index. `false` if the VM stops first.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(cpu = index))]
    pub fn wait_create_turn(&self, index: u32) -> bool {
        let mut table = self.lock();
        loop {
            if table.stopping.is_some() {
                return false;
            }
            if table.created >= index {
                return true;
            }
            table = self.wait(table);
        }
    }

    /// CPU `index` has called `hv_vcpu_create` (successfully or not).
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(cpu = index))]
    pub fn mark_created(&self, index: u32) {
        let mut table = self.lock();
        table.created = table.created.max(index + 1);
        drop(table);
        self.changed.notify_all();
    }

    /// Record the vCPU behind CPU `index` so a stop can cancel it. `false` when
    /// the VM is already stopping; the caller must not run the guest.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(cpu = index))]
    pub fn register(&self, index: u32, stop: VcpuStop) -> bool {
        let mut table = self.lock();
        if table.stopping.is_some() {
            return false;
        }
        if let Some(slot) = table.stops.get_mut(index as usize) {
            *slot = Some(stop);
        }
        true
    }

    /// Forget CPU `index`'s vCPU. Call before destroying it.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(cpu = index))]
    pub fn deregister(&self, index: u32) {
        if let Some(slot) = self.lock().stops.get_mut(index as usize) {
            *slot = None;
        }
    }

    /// CPU `index` has created its vCPU and set its MPIDR.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(cpu = index))]
    pub fn mark_ready(&self, index: u32) {
        if let Some(ready) = self.lock().ready.get_mut(index as usize) {
            *ready = true;
        }
        self.changed.notify_all();
    }

    /// Block until every CPU is ready. `false` if the VM stops first.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all)]
    pub fn wait_all_ready(&self) -> bool {
        let mut table = self.lock();
        loop {
            if table.stopping.is_some() {
                return false;
            }
            if table.ready.iter().all(|ready| *ready) {
                return true;
            }
            table = self.wait(table);
        }
    }

    /// Block until `CPU_ON` targets CPU `index`, then mark it on and return the
    /// entry point and context id. `None` when the VM stops.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(cpu = index))]
    pub fn wait_for_on(&self, index: u32) -> Option<(u64, u64)> {
        let mut table = self.lock();
        loop {
            if table.stopping.is_some() {
                return None;
            }
            if let Some(CpuState::OnPending { entry, context }) =
                table.cpus.get(index as usize).copied()
            {
                table.cpus[index as usize] = CpuState::On;
                tracing::info!(
                    target: "ternvale::psci",
                    cpu = index,
                    entry = format!("{entry:#x}"),
                    context = format!("{context:#x}"),
                    "cpu powered on"
                );
                return Some((entry, context));
            }
            table = self.wait(table);
        }
    }

    /// Answer a PSCI power request from CPU `caller`. Returns x0.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(cpu = caller, ?request))]
    pub fn handle(&self, caller: u32, request: PowerRequest) -> u64 {
        let code = match request {
            PowerRequest::CpuOn {
                target,
                entry,
                context,
            } => self.cpu_on(caller, target, entry, context),
            PowerRequest::AffinityInfo { target, level } => self.affinity_info(target, level),
        };
        tracing::debug!(target: "ternvale::psci", cpu = caller, ?request, code, "psci power request");
        sign(code)
    }

    /// CPU `index` called `CPU_OFF`. Stops the VM with [`ExitReason::CpuOff`]
    /// when no CPU is left on.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(cpu = index))]
    pub fn cpu_off(&self, index: u32) {
        let mut table = self.lock();
        if let Some(state) = table.cpus.get_mut(index as usize) {
            *state = CpuState::Off;
        }
        tracing::info!(target: "ternvale::psci", cpu = index, "cpu powered off");
        if table.cpus.iter().all(|state| *state == CpuState::Off) {
            tracing::info!(target: "ternvale::psci", "every cpu is off; stopping the vm");
            Self::stop_locked(&mut table, ExitReason::CpuOff);
        }
        drop(table);
        self.changed.notify_all();
    }

    /// Stop the VM: record `reason` (the first one wins), cancel every
    /// registered vCPU, and wake every waiting thread.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all, fields(?reason))]
    pub fn request_stop(&self, reason: ExitReason) {
        let mut table = self.lock();
        Self::stop_locked(&mut table, reason);
        drop(table);
        self.changed.notify_all();
    }

    /// Leave `hv_vcpu_run` on CPU `index` without stopping it, if its vCPU is
    /// registered. Used to deliver host input and re-pulsed SPIs.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(cpu = index))]
    pub fn nudge(&self, index: u32) {
        let table = self.lock();
        let Some(Some(stop)) = table.stops.get(index as usize) else {
            return;
        };
        if let Err(error) = stop.nudge() {
            tracing::debug!(target: "ternvale::vcpu", cpu = index, error = %error, "vcpu nudge failed");
        }
    }

    /// Why the VM is stopping, once [`CpuPower::request_stop`] has run.
    #[tracing::instrument(level = "debug", target = "ternvale::psci", skip_all)]
    pub fn stop_reason(&self) -> Option<ExitReason> {
        self.lock().stopping
    }

    fn stop_locked(table: &mut Table, reason: ExitReason) {
        if let Some(first) = table.stopping {
            tracing::debug!(target: "ternvale::psci", ?first, ?reason, "vm already stopping");
        } else {
            table.stopping = Some(reason);
            tracing::info!(target: "ternvale::psci", ?reason, "stopping every vcpu");
        }
        // Held across hv_vcpus_exit so no vCPU is destroyed between collecting
        // its id and the call (deregister takes this lock first).
        let stops: Vec<VcpuStop> = table.stops.iter().flatten().cloned().collect();
        if let Err(error) = VcpuStop::request_all(&stops) {
            tracing::error!(target: "ternvale::vcpu", error = %error, "hv_vcpus_exit on all vcpus failed");
        }
    }

    fn cpu_on(&self, caller: u32, target: u64, entry: u64, context: u64) -> i32 {
        let Some(index) = self.index_of(target) else {
            tracing::warn!(
                target: "ternvale::psci",
                cpu = caller,
                mpidr = format!("{target:#x}"),
                "cpu_on of an unknown mpidr"
            );
            return INVALID_PARAMS;
        };
        if entry % 4 != 0 || entry < self.ram_base || entry >= self.ram_end {
            tracing::warn!(
                target: "ternvale::psci",
                cpu = caller,
                entry = format!("{entry:#x}"),
                "cpu_on entry point is not 4-byte aligned guest ram"
            );
            return INVALID_ADDRESS;
        }
        let mut table = self.lock();
        let code = match table.cpus[index as usize] {
            CpuState::On => ALREADY_ON,
            CpuState::OnPending { .. } => ON_PENDING,
            CpuState::Off => {
                table.cpus[index as usize] = CpuState::OnPending { entry, context };
                SUCCESS
            }
        };
        drop(table);
        if code == SUCCESS {
            tracing::info!(
                target: "ternvale::psci",
                cpu = caller,
                target_cpu = index,
                entry = format!("{entry:#x}"),
                context = format!("{context:#x}"),
                "cpu_on accepted"
            );
            self.changed.notify_all();
        }
        code
    }

    // TODO(verify): PSCI (DEN0022) lets an implementation refuse
    // lowest_affinity_level > 0; confirm INVALID_PARAMETERS is the required code.
    fn affinity_info(&self, target: u64, level: u64) -> i32 {
        if level != 0 {
            tracing::warn!(target: "ternvale::psci", level, "affinity_info above level 0 is not supported");
            return INVALID_PARAMS;
        }
        let Some(index) = self.index_of(target) else {
            tracing::warn!(target: "ternvale::psci", mpidr = format!("{target:#x}"), "affinity_info of an unknown mpidr");
            return INVALID_PARAMS;
        };
        match self.lock().cpus[index as usize] {
            CpuState::On => AFFINITY_ON,
            CpuState::Off => AFFINITY_OFF,
            CpuState::OnPending { .. } => AFFINITY_ON_PENDING,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Table> {
        crate::lockwatch::lock(&self.table, "cpu-power")
    }

    fn wait<'a>(&self, guard: MutexGuard<'a, Table>) -> MutexGuard<'a, Table> {
        self.changed
            .wait(guard)
            .unwrap_or_else(|poison| poison.into_inner())
    }
}

#[cfg(test)]
#[path = "smp_tests.rs"]
mod tests;
