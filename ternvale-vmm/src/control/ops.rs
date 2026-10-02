//! [`VmControl`] requests (pause, resume, stop), machine-side transitions,
//! and status snapshots.

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use super::{
    ControlError, CpuStats, Inner, StopCause, VmControl, VmState, VmStats, VmStatus, KICK_EVERY,
    PARK_POLL,
};
use crate::lockwatch::Guard;
use crate::vcpu::ExitReason;

fn millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

impl VmControl {
    /// Stop every vCPU outside the guest. Returns once all have left
    /// `hv_vcpu_run`, or withdraws the request after `timeout`. Pausing a
    /// paused VM succeeds without doing anything.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vm = %self.name, timeout_ms = millis(timeout)))]
    pub fn pause(&self, timeout: Duration) -> Result<VmStatus, ControlError> {
        let started = Instant::now();
        {
            let inner = self.lock();
            match inner.state {
                VmState::Running => {}
                VmState::Paused => {
                    tracing::info!(target: "ternvale::vcpu", vm = %self.name, "pause requested; already paused");
                    drop(inner);
                    return Ok(self.status());
                }
                state => return Err(self.reject("pause", state)),
            }
            self.pause_requested.store(true, Ordering::SeqCst);
        }
        tracing::info!(target: "ternvale::vcpu", vm = %self.name, "pause requested; kicking vcpus");
        let mut kicks = 0u32;
        loop {
            let busy = self.busy_cpus();
            if busy.is_empty() {
                break;
            }
            if self.is_stopping() {
                self.withdraw_pause();
                return Err(ControlError::StoppedWhilePausing);
            }
            if started.elapsed() >= timeout {
                self.withdraw_pause();
                tracing::warn!(target: "ternvale::vcpu", vm = %self.name, cpus = ?busy, kicks, "pause timed out; resuming");
                return Err(ControlError::PauseTimeout {
                    waited_ms: millis(started.elapsed()),
                    cpus: busy,
                });
            }
            self.call_kick();
            kicks += 1;
            let (_inner, _timed_out) = self.lock().wait_timeout(&self.changed, KICK_EVERY);
        }
        let mut inner = self.lock();
        if !self.transition(&mut inner, VmState::Paused, "every vcpu left the guest") {
            let state = inner.state;
            drop(inner);
            self.withdraw_pause();
            return Err(match state {
                VmState::Stopping | VmState::Stopped | VmState::Failed => {
                    ControlError::StoppedWhilePausing
                }
                state => self.reject("pause", state),
            });
        }
        inner.paused_at = Some((Instant::now(), (self.clock)()));
        inner.pauses += 1;
        drop(inner);
        tracing::info!(
            target: "ternvale::vcpu",
            vm = %self.name,
            kicks,
            pause_ms = millis(started.elapsed()),
            "vm paused"
        );
        Ok(self.status())
    }

    /// Let parked vCPUs run again. Resuming a running VM succeeds without
    /// doing anything.
    #[tracing::instrument(level = "debug", target = "ternvale::vcpu", skip_all, fields(vm = %self.name))]
    pub fn resume(&self) -> Result<VmStatus, ControlError> {
        let mut inner = self.lock();
        match inner.state {
            VmState::Paused => {}
            VmState::Running => {
                tracing::info!(target: "ternvale::vcpu", vm = %self.name, "resume requested; already running");
                drop(inner);
                return Ok(self.status());
            }
            state => return Err(self.reject("resume", state)),
        }
        let ticks = self.account_pause(&mut inner);
        self.pause_requested.store(false, Ordering::SeqCst);
        self.transition(&mut inner, VmState::Running, "resume");
        drop(inner);
        self.changed.notify_all();
        tracing::info!(target: "ternvale::vcpu", vm = %self.name, paused_ticks = ticks, total_ticks = self.paused_ticks(), "vm resumed");
        Ok(self.status())
    }

    /// Orderly stop (`force` false) or forced stop. A second request while
    /// stopping only upgrades to forced.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name, force))]
    pub fn request_stop(&self, force: bool) -> Result<VmStatus, ControlError> {
        let cause = if force {
            StopCause::ForceStop
        } else {
            StopCause::Shutdown
        };
        let mut inner = self.lock();
        match inner.state {
            VmState::Created | VmState::Running | VmState::Paused => {
                if inner.state == VmState::Paused {
                    self.account_pause(&mut inner);
                }
                inner.cause = Some(cause);
                self.pause_requested.store(false, Ordering::SeqCst);
                self.transition(
                    &mut inner,
                    VmState::Stopping,
                    if force {
                        "force-stop requested"
                    } else {
                        "shutdown requested"
                    },
                );
            }
            VmState::Stopping => {
                tracing::info!(target: "ternvale::boot", vm = %self.name, force, "stop requested; already stopping");
            }
            state => return Err(self.reject(if force { "force-stop" } else { "shut down" }, state)),
        }
        if force && inner.forced_at.is_none() {
            inner.forced_at = Some(Instant::now());
            inner.cause = Some(StopCause::ForceStop);
        }
        drop(inner);
        self.changed.notify_all();
        self.call_stop();
        Ok(self.status())
    }

    /// Time since `force-stop` was requested.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name))]
    pub fn forced_for(&self) -> Option<Duration> {
        self.lock().forced_at.map(|at| at.elapsed())
    }

    /// Block until the state is terminal or `timeout` passes; returns the state.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name, timeout_ms = millis(timeout)))]
    pub fn wait_terminal(&self, timeout: Duration) -> VmState {
        let deadline = Instant::now() + timeout;
        let mut inner = self.lock();
        while !inner.state.is_terminal() {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                break;
            }
            inner = inner.wait_timeout(&self.changed, left.min(PARK_POLL)).0;
        }
        inner.state
    }

    /// The machine started its vCPU threads.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name))]
    pub fn mark_running(&self) {
        let mut inner = self.lock();
        if inner.state == VmState::Created {
            self.transition(&mut inner, VmState::Running, "vcpu threads started");
        }
    }

    /// The run is ending for `reason` (guest power-off, error, or a stop
    /// already requested here).
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name))]
    pub fn mark_stopping(&self, reason: ExitReason) {
        let mut inner = self.lock();
        if matches!(
            inner.state,
            VmState::Created | VmState::Running | VmState::Paused
        ) {
            if inner.state == VmState::Paused {
                self.account_pause(&mut inner);
            }
            inner.cause.get_or_insert(StopCause::Guest(reason));
            self.pause_requested.store(false, Ordering::SeqCst);
            self.transition(&mut inner, VmState::Stopping, "vcpus stopped");
        }
        drop(inner);
        self.changed.notify_all();
    }

    /// The machine returned: `Ok` → `Stopped`, `Err` → `Failed`.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name, ok = result.is_ok()))]
    pub fn finish(&self, result: Result<ExitReason, String>) {
        let mut inner = self.lock();
        self.pause_requested.store(false, Ordering::SeqCst);
        match result {
            Ok(reason) => {
                if inner.state != VmState::Stopping {
                    inner.cause.get_or_insert(StopCause::Guest(reason));
                    self.transition(&mut inner, VmState::Stopping, "machine returned");
                }
                self.transition(&mut inner, VmState::Stopped, "vm torn down");
            }
            Err(error) => {
                tracing::error!(target: "ternvale::boot", vm = %self.name, error = %error, "vm failed");
                inner.failure = Some(error);
                self.transition(&mut inner, VmState::Failed, "machine error");
            }
        }
        drop(inner);
        self.changed.notify_all();
    }

    /// Current state and counters.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name))]
    pub fn status(&self) -> VmStatus {
        let inner = self.lock();
        self.status_locked(&inner)
    }

    /// [`VmControl::status`] plus per-CPU counters and process CPU time.
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(vm = %self.name))]
    pub fn stats(&self) -> VmStats {
        let inner = self.lock();
        let status = self.status_locked(&inner);
        let cpus = (0..self.cpus)
            .map(|cpu| CpuStats {
                cpu,
                in_guest: self.in_guest[cpu as usize].load(Ordering::SeqCst),
                stats: inner.sources[cpu as usize]
                    .as_ref()
                    .map(|source| source.snapshot()),
            })
            .collect();
        drop(inner);
        VmStats {
            status,
            cpus,
            process_cpu_ms: crate::vcpu::process_cpu_ms(),
        }
    }

    fn status_locked(&self, inner: &Inner) -> VmStatus {
        let current = inner
            .paused_at
            .map_or(Duration::ZERO, |(at, _)| at.elapsed());
        VmStatus {
            name: self.name.clone(),
            state: inner.state,
            cpus: self.cpus,
            uptime_ms: millis(self.created.elapsed()),
            state_ms: millis(inner.since.elapsed()),
            paused_ms: millis(inner.paused + current),
            pauses: inner.pauses,
            stop_cause: inner.cause,
            failure: inner.failure.clone(),
        }
    }

    pub(super) fn park(&self, cpu: u32) -> bool {
        tracing::debug!(target: "ternvale::vcpu", cpu, "vcpu parked for pause");
        loop {
            if self.is_stopping() {
                tracing::debug!(target: "ternvale::vcpu", cpu, "parked vcpu sees the machine stopping");
                return false;
            }
            let inner = self.lock();
            if matches!(
                inner.state,
                VmState::Stopping | VmState::Stopped | VmState::Failed
            ) {
                return false;
            }
            if !self.pause_requested.load(Ordering::SeqCst) {
                tracing::debug!(target: "ternvale::vcpu", cpu, "vcpu unparked");
                return true;
            }
            let (_inner, _timed_out) = inner.wait_timeout(&self.changed, PARK_POLL);
        }
    }

    pub(super) fn notify_pauser(&self) {
        let _inner = self.lock();
        self.changed.notify_all();
    }

    pub(super) fn call_stop(&self) {
        match self.hooks.get() {
            Some(hooks) => (hooks.stop)(),
            None => {
                tracing::debug!(target: "ternvale::boot", vm = %self.name, "stop recorded; machine not bound yet")
            }
        }
    }

    fn call_kick(&self) {
        if let Some(hooks) = self.hooks.get() {
            (hooks.kick)();
        }
    }

    fn is_stopping(&self) -> bool {
        self.hooks.get().is_some_and(|hooks| (hooks.stopping)())
    }

    fn busy_cpus(&self) -> Vec<u32> {
        (0..self.cpus)
            .filter(|&cpu| self.in_guest[cpu as usize].load(Ordering::SeqCst))
            .collect()
    }

    fn withdraw_pause(&self) {
        self.pause_requested.store(false, Ordering::SeqCst);
        let _inner = self.lock();
        self.changed.notify_all();
        tracing::info!(target: "ternvale::vcpu", vm = %self.name, "pause withdrawn");
    }

    /// Close the pause in progress: add its wall time and host ticks.
    fn account_pause(&self, inner: &mut Inner) -> u64 {
        let Some((at, start)) = inner.paused_at.take() else {
            return 0;
        };
        inner.paused += at.elapsed();
        let ticks = (self.clock)().saturating_sub(start);
        self.paused_ticks.fetch_add(ticks, Ordering::AcqRel);
        ticks
    }

    fn reject(&self, op: &'static str, state: VmState) -> ControlError {
        let needs = match op {
            "pause" => "running (or already paused)",
            "resume" => "paused (or already running)",
            _ => "created, running, paused, or already stopping",
        };
        tracing::warn!(target: "ternvale::boot", vm = %self.name, op, %state, needs, "control request rejected");
        ControlError::InvalidState { op, state, needs }
    }

    /// Apply `inner.state → to`, logging it. `false` (and WARN) if not allowed.
    pub(super) fn transition(&self, inner: &mut Guard<'_, Inner>, to: VmState, why: &str) -> bool {
        let from = inner.state;
        if !from.allows(to) {
            tracing::warn!(target: "ternvale::boot", vm = %self.name, %from, %to, why, "rejected vm state transition");
            return false;
        }
        let held_ms = millis(inner.since.elapsed());
        inner.state = to;
        inner.since = Instant::now();
        tracing::info!(target: "ternvale::boot", vm = %self.name, %from, %to, why, held_ms, "vm state");
        self.changed.notify_all();
        true
    }
}
