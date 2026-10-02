//! VM lifecycle state machine and vCPU pause/resume.
//!
//! States: `Created → Running ⇄ Paused`, then `Stopping → Stopped`. `Failed`
//! is reachable from every state that is not terminal. Every transition is
//! logged at INFO on `ternvale::boot`; a rejected one at WARN.
//!
//! Pause sets `pause_requested`, then kicks every vCPU out of `hv_vcpu_run`
//! with `hv_vcpus_exit` (the machine's kick hook) until no vCPU is inside the
//! guest. vCPU threads call [`VmControl::enter_guest`] before each run and
//! block there while a pause is requested, and [`VmControl::leave_guest`]
//! after it. Both sides use `SeqCst`, so either the pauser sees the vCPU in
//! the guest (and kicks it again) or the vCPU sees the request (and parks).
//!
//! Resume adds the host ticks spent paused to [`VmControl::paused_ticks`].
//! Each vCPU adds the part it has not applied yet to its vtimer offset before
//! its next run, so the guest's `CNTVCT_EL0` does not jump across a pause.
//!
//! The `inner` mutex is a leaf: hooks are never called while it is held.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Condvar, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::diag::Diagnostics;
use crate::lockwatch::Guard;
use crate::vcpu::{ExitReason, VcpuStats, VcpuStatsSource};

/// How long a parked vCPU sleeps before re-checking for a stop that did not
/// go through this controller (a vCPU error calls `CpuPower` directly).
const PARK_POLL: Duration = Duration::from_millis(50);
/// Re-kick interval while a pause waits for vCPUs to leave the guest.
const KICK_EVERY: Duration = Duration::from_millis(5);

/// Lifecycle state of one VM.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmState {
    /// Built, vCPU threads not started yet.
    Created,
    /// vCPUs run the guest.
    Running,
    /// Every vCPU is parked outside `hv_vcpu_run`.
    Paused,
    /// A stop was requested or the guest powered off; vCPUs are winding down.
    Stopping,
    /// The VM was torn down cleanly.
    Stopped,
    /// Setup or a vCPU failed.
    Failed,
}

impl VmState {
    /// Lower-case name used in logs and on the control socket.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Paused => "paused",
            Self::Stopping => "stopping",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }

    /// `Stopped` or `Failed`: no transition leaves these.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Stopped | Self::Failed)
    }

    /// Whether `self → to` is an allowed transition.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn allows(self, to: VmState) -> bool {
        use VmState::*;
        matches!(
            (self, to),
            (Created, Running | Stopping | Failed)
                | (Running, Paused | Stopping | Failed)
                | (Paused, Running | Stopping | Failed)
                | (Stopping, Stopped | Failed)
        )
    }
}

impl std::fmt::Display for VmState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Why the VM left `Running`/`Paused`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopCause {
    /// Control `shutdown`: orderly host-side stop.
    Shutdown,
    /// Control `force-stop`: stop, and the host process may exit without
    /// waiting for teardown.
    ForceStop,
    /// The guest (PSCI) or a vCPU error ended the run.
    Guest(ExitReason),
}

impl std::fmt::Display for StopCause {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Shutdown => formatter.write_str("shutdown"),
            Self::ForceStop => formatter.write_str("force-stop"),
            Self::Guest(reason) => write!(formatter, "guest {reason:?}"),
        }
    }
}

/// A control request that cannot be carried out.
#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    /// The request does not apply in the current state.
    #[error("cannot {op} a vm that is {state}; {op} needs a vm that is {needs}")]
    InvalidState {
        /// Requested operation.
        op: &'static str,
        /// State at the time of the request.
        state: VmState,
        /// States the operation accepts, in words.
        needs: &'static str,
    },
    /// Some vCPU did not leave the guest in time; the pause was withdrawn.
    #[error("pause timed out after {waited_ms} ms; cpus still in the guest: {cpus:?}")]
    PauseTimeout {
        /// Time spent kicking.
        waited_ms: u64,
        /// CPUs still inside `hv_vcpu_run`.
        cpus: Vec<u32>,
    },
    /// The VM began stopping while a pause was in progress.
    #[error("vm began stopping while pausing")]
    StoppedWhilePausing,
}

/// Callbacks into the running machine, installed by [`VmControl::bind`].
pub struct ControlHooks {
    /// `hv_vcpus_exit` on every vCPU without stopping it.
    pub kick: Box<dyn Fn() + Send + Sync>,
    /// Stop every vCPU.
    pub stop: Box<dyn Fn() + Send + Sync>,
    /// Whether the machine is stopping for any reason.
    pub stopping: Box<dyn Fn() -> bool + Send + Sync>,
}

/// Point-in-time view for `status`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmStatus {
    /// VM name.
    pub name: String,
    /// Current state.
    pub state: VmState,
    /// Configured CPU count.
    pub cpus: u32,
    /// Time since the controller was created.
    pub uptime_ms: u64,
    /// Time in the current state.
    pub state_ms: u64,
    /// Total time paused, including a pause in progress.
    pub paused_ms: u64,
    /// Completed pause requests.
    pub pauses: u64,
    /// Why the VM stopped or is stopping.
    pub stop_cause: Option<StopCause>,
    /// Error text when `Failed`.
    pub failure: Option<String>,
}

/// One CPU in `query-stats`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuStats {
    /// Guest CPU index.
    pub cpu: u32,
    /// Inside `hv_vcpu_run` right now.
    pub in_guest: bool,
    /// Counters, once the vCPU exists.
    pub stats: Option<VcpuStats>,
}

/// `query-stats` result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VmStats {
    /// Same as [`VmControl::status`].
    pub status: VmStatus,
    /// Per-CPU counters.
    pub cpus: Vec<CpuStats>,
    /// CPU time of the whole host process.
    pub process_cpu_ms: Option<u64>,
}

struct Inner {
    state: VmState,
    since: Instant,
    paused_at: Option<(Instant, u64)>,
    paused: Duration,
    pauses: u64,
    cause: Option<StopCause>,
    failure: Option<String>,
    forced_at: Option<Instant>,
    sources: Vec<Option<VcpuStatsSource>>,
}

/// Lifecycle and pause control for one VM, shared by the machine, its vCPU
/// threads, and the control socket.
pub struct VmControl {
    name: String,
    cpus: u32,
    created: Instant,
    inner: Mutex<Inner>,
    changed: Condvar,
    pause_requested: AtomicBool,
    in_guest: Vec<AtomicBool>,
    paused_ticks: AtomicU64,
    hooks: OnceLock<ControlHooks>,
    clock: fn() -> u64,
    diag: Arc<Diagnostics>,
}

mod ops;

impl VmControl {
    /// Controller for VM `name` with `cpus` vCPUs, in `Created`.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = name, cpus))]
    pub fn new(name: &str, cpus: u32) -> Self {
        Self::with_clock(name, cpus, ternvale_hv::host_ticks)
    }

    /// [`VmControl::new`] with a different host tick source (tests).
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = name, cpus))]
    pub fn with_clock(name: &str, cpus: u32, clock: fn() -> u64) -> Self {
        let now = Instant::now();
        tracing::info!(target: "ternvale::boot", vm = name, cpus, state = %VmState::Created, "vm state initialized");
        Self {
            name: name.to_string(),
            cpus,
            created: now,
            inner: Mutex::new(Inner {
                state: VmState::Created,
                since: now,
                paused_at: None,
                paused: Duration::ZERO,
                pauses: 0,
                cause: None,
                failure: None,
                forced_at: None,
                sources: vec![None; cpus as usize],
            }),
            changed: Condvar::new(),
            pause_requested: AtomicBool::new(false),
            in_guest: (0..cpus).map(|_| AtomicBool::new(false)).collect(),
            paused_ticks: AtomicU64::new(0),
            hooks: OnceLock::new(),
            clock,
            diag: Arc::new(Diagnostics::new()),
        }
    }

    /// DTB, MMIO trace, and device counts the machine records for crash reports.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name))]
    pub fn diagnostics(&self) -> &Arc<Diagnostics> {
        &self.diag
    }

    /// VM name.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// vCPU count this controller tracks.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
    pub fn cpus(&self) -> u32 {
        self.cpus
    }

    /// Current state.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name))]
    pub fn state(&self) -> VmState {
        self.lock().state
    }

    /// Install the machine callbacks. A stop requested before this runs is
    /// forwarded now.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name))]
    pub fn bind(&self, hooks: ControlHooks) {
        if self.hooks.set(hooks).is_err() {
            tracing::warn!(target: "ternvale::boot", vm = %self.name, "control hooks already bound; keeping the first");
            return;
        }
        tracing::debug!(target: "ternvale::boot", vm = %self.name, "control hooks bound");
        if self.state() == VmState::Stopping {
            self.call_stop();
        }
    }

    /// Record the counters of CPU `cpu` for `query-stats`.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vm = %self.name, cpu))]
    pub fn attach_cpu_stats(&self, cpu: u32, source: VcpuStatsSource) {
        match self.lock().sources.get_mut(cpu as usize) {
            Some(slot) => *slot = Some(source),
            None => {
                tracing::warn!(target: "ternvale::vcpu", cpu, cpus = self.cpus, "stats for a cpu outside the vm")
            }
        }
    }

    /// Host ticks the VM has spent paused, summed over completed pauses.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all)]
    pub fn paused_ticks(&self) -> u64 {
        self.paused_ticks.load(Ordering::Acquire)
    }

    /// Called by CPU `cpu` before `hv_vcpu_run`. Blocks while a pause is
    /// requested. Returns `false` when the VM is stopping; the caller still
    /// runs, and the stopped vCPU returns at once.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(cpu))]
    pub fn enter_guest(&self, cpu: u32) -> bool {
        let Some(flag) = self.in_guest.get(cpu as usize) else {
            return true;
        };
        flag.store(true, Ordering::SeqCst);
        if !self.pause_requested.load(Ordering::SeqCst) {
            return true;
        }
        flag.store(false, Ordering::SeqCst);
        self.notify_pauser();
        let running = self.park(cpu);
        flag.store(true, Ordering::SeqCst);
        running
    }

    /// Called by CPU `cpu` after `hv_vcpu_run` returns.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(cpu))]
    pub fn leave_guest(&self, cpu: u32) {
        if let Some(flag) = self.in_guest.get(cpu as usize) {
            flag.store(false, Ordering::SeqCst);
        }
        if self.pause_requested.load(Ordering::SeqCst) {
            self.notify_pauser();
        }
    }

    fn lock(&self) -> Guard<'_, Inner> {
        crate::lockwatch::lock(&self.inner, "vm-control")
    }
}

#[cfg(test)]
#[path = "control_tests.rs"]
mod tests;
