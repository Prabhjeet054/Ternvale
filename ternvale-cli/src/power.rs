//! Host sleep/wake, graceful guest shutdown, quit, and window-hidden pause.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ternvale_agent_proto::Message;
use ternvale_config::{OnQuit, PowerSection};
use ternvale_devices::{AgentServer, AgentState};
use ternvale_hv::HostPowerEvent;
use ternvale_vmm::{VmControl, VmState};

use crate::protocol::{Request, Response, DEFAULT_PAUSE_MS};

/// Shared `[power]` policy and in-flight pause reasons.
pub struct PowerState {
    /// Config table.
    pub policy: PowerSection,
    sleep_held: AtomicBool,
    hidden_held: AtomicBool,
}

impl PowerState {
    /// Policy from the VM config.
    #[tracing::instrument(level = "debug", target = "ternvale::power", skip_all)]
    pub fn new(policy: PowerSection) -> Arc<Self> {
        if policy.low_power {
            tracing::info!(target: "ternvale::power", "low-power setting on (pause when window is hidden)");
        }
        Arc::new(Self {
            policy,
            sleep_held: AtomicBool::new(false),
            hidden_held: AtomicBool::new(false),
        })
    }

    /// IOKit / SIGTERM callback.
    #[tracing::instrument(level = "debug", target = "ternvale::power", skip_all)]
    pub fn on_host_event(
        &self,
        event: HostPowerEvent,
        control: &VmControl,
        agent: Option<Arc<AgentServer>>,
    ) {
        match event {
            HostPowerEvent::WillSleep => self.on_sleep(control),
            HostPowerEvent::DidWake => self.on_wake(control, agent),
            HostPowerEvent::QuitRequested => self.on_quit(control, agent.as_deref()),
        }
    }

    /// Pause vCPUs so host sleep does not run the guest; vtimer offset hides
    /// the pause (`hv_vcpu_set_vtimer_offset` / CNTVCT = mach_absolute_time
    /// − offset). TODO(verify): offset after a real Mac sleep vs pause-only.
    #[tracing::instrument(level = "debug", target = "ternvale::power", skip_all, fields(vm = control.name()))]
    pub fn on_sleep(&self, control: &VmControl) {
        tracing::info!(target: "ternvale::power", vm = control.name(), "host sleep: pausing vcpus");
        match control.pause(Duration::from_millis(DEFAULT_PAUSE_MS)) {
            Ok(_) => {
                self.sleep_held.store(true, Ordering::SeqCst);
            }
            Err(error) => {
                tracing::warn!(target: "ternvale::power", error = %error, "host sleep pause failed");
            }
        }
    }

    /// Resume and ask the agent to set the wall clock.
    #[tracing::instrument(level = "debug", target = "ternvale::power", skip_all, fields(vm = control.name()))]
    pub fn on_wake(&self, control: &VmControl, agent: Option<Arc<AgentServer>>) {
        tracing::info!(target: "ternvale::power", vm = control.name(), "host wake: resuming vcpus");
        if self.sleep_held.swap(false, Ordering::SeqCst) {
            if let Err(error) = control.resume() {
                tracing::warn!(target: "ternvale::power", error = %error, "host wake resume failed");
            }
        }
        schedule_time_sync(agent);
    }

    /// SIGINT/SIGTERM / app quit.
    #[tracing::instrument(level = "debug", target = "ternvale::power", skip_all, fields(vm = control.name()))]
    pub fn on_quit(&self, control: &VmControl, agent: Option<&AgentServer>) {
        match self.policy.on_quit {
            OnQuit::Suspend => {
                tracing::info!(
                    target: "ternvale::power",
                    vm = control.name(),
                    "app quit: suspending (pause vcpus)"
                );
                if let Err(error) = control.pause(Duration::from_millis(DEFAULT_PAUSE_MS)) {
                    tracing::warn!(target: "ternvale::power", error = %error, "quit suspend pause failed");
                }
            }
            OnQuit::Shutdown => {
                tracing::info!(
                    target: "ternvale::power",
                    vm = control.name(),
                    "app quit: graceful shutdown"
                );
                let _ = graceful_stop(control, agent, self.policy.graceful_timeout_secs);
            }
        }
    }

    /// `window-hidden` from the app. Honoured when `pause_when_hidden` or
    /// `low_power` is set.
    #[tracing::instrument(level = "debug", target = "ternvale::power", skip_all, fields(vm = control.name(), hidden))]
    pub fn on_window_hidden(&self, control: &VmControl, hidden: bool) -> Result<Response, String> {
        if !self.policy.pause_when_hidden() {
            tracing::debug!(target: "ternvale::power", "window-hidden ignored (pause_when_hidden off)");
            return Ok(Response::status(&control.status()));
        }
        if hidden {
            tracing::info!(target: "ternvale::power", vm = control.name(), "window hidden: pausing vcpus");
            control
                .pause(Duration::from_millis(DEFAULT_PAUSE_MS))
                .map(|status| {
                    self.hidden_held.store(true, Ordering::SeqCst);
                    Response::status(&status)
                })
                .map_err(|error| error.to_string())
        } else {
            tracing::info!(target: "ternvale::power", vm = control.name(), "window shown: resuming vcpus");
            if !self.hidden_held.swap(false, Ordering::SeqCst) {
                return Ok(Response::status(&control.status()));
            }
            if self.sleep_held.load(Ordering::SeqCst) {
                return Ok(Response::status(&control.status()));
            }
            control
                .resume()
                .map(|status| Response::status(&status))
                .map_err(|error| error.to_string())
        }
    }
}

/// Pulse the guest power button, optionally tell the agent, wait, then force-stop.
#[tracing::instrument(level = "debug", target = "ternvale::power", skip_all, fields(vm = control.name(), timeout_secs))]
pub fn graceful_stop(
    control: &VmControl,
    agent: Option<&AgentServer>,
    timeout_secs: u64,
) -> Result<Response, String> {
    tracing::info!(
        target: "ternvale::power",
        vm = control.name(),
        timeout_secs,
        "graceful-stop: power button"
    );
    if !control.press_power_button() {
        tracing::warn!(
            target: "ternvale::power",
            "no GED/PL061 button; waiting for an already-stopping guest"
        );
    }
    if let Some(agent) = agent {
        match agent.send(&Message::Shutdown) {
            Ok(()) => tracing::info!(target: "ternvale::power", "sent agent shutdown"),
            Err(error) => tracing::debug!(
                target: "ternvale::power",
                error = %error,
                "agent shutdown not sent"
            ),
        }
    }
    let timeout = Duration::from_secs(timeout_secs.max(1));
    let state = wait_guest_poweroff(control, timeout);
    if matches!(
        state,
        VmState::Stopping | VmState::Stopped | VmState::Failed
    ) {
        tracing::info!(target: "ternvale::power", state = %state, "guest accepted graceful-stop");
        // Guest PSCI/agent power-off leaves the VM `Stopping` until the
        // machine tears down; `request_stop` runs the host stop hook so
        // `ternvale stop --graceful` can wait for the process to exit.
        match control.request_stop(false) {
            Ok(status) => return Ok(Response::status(&status)),
            Err(error) => tracing::debug!(
                target: "ternvale::power",
                error = %error,
                "host stop after guest power-off"
            ),
        }
        return Ok(Response::status(&control.status()));
    }
    tracing::info!(
        target: "ternvale::power",
        state = %state,
        timeout_secs,
        "graceful-stop timed out; force-stop"
    );
    control
        .request_stop(true)
        .map(|status| Response::status(&status))
        .map_err(|error| error.to_string())
}

/// `wait_terminal` is only `Stopped`/`Failed`. Guest power-off is `Stopping`.
fn wait_guest_poweroff(control: &VmControl, timeout: Duration) -> VmState {
    let deadline = Instant::now() + timeout;
    loop {
        let state = control.state();
        if matches!(
            state,
            VmState::Stopping | VmState::Stopped | VmState::Failed
        ) {
            return state;
        }
        if Instant::now() >= deadline {
            return state;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// Pause drops the vsock agent; wait for reconnect before `time_sync`.
fn schedule_time_sync(agent: Option<Arc<AgentServer>>) {
    let Some(agent) = agent else {
        return;
    };
    let worker = Arc::clone(&agent);
    match std::thread::Builder::new()
        .name("time-sync".into())
        .spawn(move || {
            let _status = worker.wait_for(Duration::from_secs(8), |status| {
                status.state == AgentState::Connected
            });
            time_sync(Some(&worker));
        }) {
        Ok(_) => {}
        Err(error) => {
            tracing::warn!(target: "ternvale::power", error = %error, "time_sync thread spawn failed");
            time_sync(Some(&agent));
        }
    }
}

fn time_sync(agent: Option<&AgentServer>) {
    let Some(agent) = agent else {
        return;
    };
    let unix_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    match agent.send(&Message::TimeSync { unix_secs }) {
        Ok(()) => tracing::info!(target: "ternvale::power", unix_secs, "sent agent time_sync"),
        Err(error) => tracing::debug!(
            target: "ternvale::power",
            error = %error,
            "agent time_sync not sent"
        ),
    }
}

/// Dispatch control-socket power commands.
#[tracing::instrument(level = "debug", target = "ternvale::power", skip_all)]
pub fn handle(
    request: &Request,
    control: &VmControl,
    agent: Option<&AgentServer>,
    power: Option<&PowerState>,
) -> Option<Result<Response, String>> {
    match request {
        Request::GracefulStop { timeout_ms } => {
            let secs = timeout_ms
                .map(|ms| (ms / 1000).max(1))
                .or_else(|| power.map(|p| p.policy.graceful_timeout_secs))
                .unwrap_or(ternvale_config::DEFAULT_GRACEFUL_TIMEOUT_SECS);
            Some(graceful_stop(control, agent, secs))
        }
        Request::WindowHidden { hidden } => {
            let Some(power) = power else {
                return Some(Err("this vm has no [power] policy".into()));
            };
            Some(power.on_window_hidden(control, *hidden))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_vmm::VmControl;

    #[test]
    fn graceful_stop_without_button_force_stops_a_created_vm() {
        let control = VmControl::new("pwr", 1);
        let response = graceful_stop(&control, None, 1).expect("stop");
        assert!(response.ok, "{response:?}");
        assert_eq!(control.state(), VmState::Stopping);
    }

    #[test]
    fn graceful_stop_force_stops_when_a_running_guest_ignores_the_button() {
        use std::time::Instant;
        use ternvale_vmm::StopCause;

        let control = VmControl::new("pwr-run", 1);
        control.mark_running();
        let started = Instant::now();
        let response = graceful_stop(&control, None, 1).expect("stop");
        assert!(response.ok, "{response:?}");
        assert!(
            started.elapsed() >= Duration::from_millis(900),
            "timeout was not waited: {:?}",
            started.elapsed()
        );
        assert_eq!(control.state(), VmState::Stopping);
        assert_eq!(control.status().stop_cause, Some(StopCause::ForceStop));
    }
}
