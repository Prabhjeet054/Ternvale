use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use super::{ControlError, ControlHooks, StopCause, VmControl, VmState};
use crate::vcpu::{ExitReason, VcpuStatsSource};

static FAKE_TICKS: AtomicU64 = AtomicU64::new(1_000);

fn fake_clock() -> u64 {
    FAKE_TICKS.load(Ordering::SeqCst)
}

/// Hooks for a machine with `kicked` as every vCPU's exit flag.
fn hooks(kicked: Arc<AtomicBool>, stopped: Arc<AtomicBool>) -> ControlHooks {
    let stop_flag = Arc::clone(&stopped);
    ControlHooks {
        kick: Box::new(move || kicked.store(true, Ordering::SeqCst)),
        stop: Box::new(move || stop_flag.store(true, Ordering::SeqCst)),
        stopping: Box::new(move || stopped.load(Ordering::SeqCst)),
    }
}

/// A fake vCPU: "runs" in 1 ms slices until kicked, counting runs, until the
/// VM stops.
fn fake_vcpu(
    control: Arc<VmControl>,
    cpu: u32,
    kicked: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    runs: Arc<AtomicU64>,
) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || loop {
        let running = control.enter_guest(cpu);
        if !running || stopped.load(Ordering::SeqCst) {
            control.leave_guest(cpu);
            return;
        }
        for _ in 0..20 {
            if kicked.swap(false, Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_micros(50));
        }
        runs.fetch_add(1, Ordering::SeqCst);
        control.leave_guest(cpu);
    })
}

#[test]
fn transition_table_matches_the_lifecycle() {
    use VmState::*;
    let all = [Created, Running, Paused, Stopping, Stopped, Failed];
    let allowed = [
        (Created, Running),
        (Created, Stopping),
        (Created, Failed),
        (Running, Paused),
        (Running, Stopping),
        (Running, Failed),
        (Paused, Running),
        (Paused, Stopping),
        (Paused, Failed),
        (Stopping, Stopped),
        (Stopping, Failed),
    ];
    for from in all {
        for to in all {
            assert_eq!(
                from.allows(to),
                allowed.contains(&(from, to)),
                "{from} -> {to}"
            );
        }
    }
    assert!(Stopped.is_terminal() && Failed.is_terminal() && !Stopping.is_terminal());
}

#[test]
fn walks_created_running_paused_running_stopping_stopped() {
    let control = VmControl::with_clock("t", 1, fake_clock);
    assert_eq!(control.state(), VmState::Created);
    assert!(matches!(
        control.pause(Duration::from_millis(10)),
        Err(ControlError::InvalidState {
            op: "pause",
            state: VmState::Created,
            ..
        })
    ));
    control.mark_running();
    assert_eq!(
        control.resume().expect("resume running").state,
        VmState::Running
    );
    assert_eq!(
        control
            .pause(Duration::from_millis(100))
            .expect("pause")
            .state,
        VmState::Paused
    );
    assert_eq!(
        control
            .pause(Duration::from_millis(100))
            .expect("pause again")
            .pauses,
        1
    );
    assert_eq!(control.resume().expect("resume").state, VmState::Running);
    let status = control.request_stop(false).expect("shutdown");
    assert_eq!(status.state, VmState::Stopping);
    assert_eq!(status.stop_cause, Some(StopCause::Shutdown));
    control.finish(Ok(ExitReason::Canceled));
    let status = control.status();
    assert_eq!(status.state, VmState::Stopped);
    assert_eq!(status.stop_cause, Some(StopCause::Shutdown));
    assert!(control.request_stop(true).is_err());
    assert_eq!(
        control.wait_terminal(Duration::from_millis(1)),
        VmState::Stopped
    );
}

#[test]
fn requests_on_a_stopped_vm_fail_with_the_state_and_what_is_needed() {
    let control = VmControl::with_clock("t", 1, fake_clock);
    control.mark_running();
    control.request_stop(false).expect("shutdown");
    control.finish(Ok(ExitReason::Canceled));
    let pause = control
        .pause(Duration::from_millis(10))
        .expect_err("pause stopped");
    assert_eq!(
        pause.to_string(),
        "cannot pause a vm that is stopped; pause needs a vm that is running (or already paused)"
    );
    let resume = control.resume().expect_err("resume stopped");
    assert_eq!(
        resume.to_string(),
        "cannot resume a vm that is stopped; resume needs a vm that is paused (or already running)"
    );
    let stop = control.request_stop(false).expect_err("shutdown stopped");
    assert!(
        stop.to_string()
            .starts_with("cannot shut down a vm that is stopped;"),
        "{stop}"
    );
    assert_eq!(
        control.state(),
        VmState::Stopped,
        "rejections change nothing"
    );
}

#[test]
fn pause_parks_every_vcpu_and_resume_releases_them() {
    let control = Arc::new(VmControl::with_clock("t", 2, fake_clock));
    let (kicked, stopped) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    control.bind(hooks(Arc::clone(&kicked), Arc::clone(&stopped)));
    control.mark_running();
    let runs = Arc::new(AtomicU64::new(0));
    let threads: Vec<_> = (0..2)
        .map(|cpu| {
            fake_vcpu(
                Arc::clone(&control),
                cpu,
                Arc::clone(&kicked),
                Arc::clone(&stopped),
                Arc::clone(&runs),
            )
        })
        .collect();
    std::thread::sleep(Duration::from_millis(20));
    control.pause(Duration::from_secs(2)).expect("pause");
    assert!(control.stats().cpus.iter().all(|cpu| !cpu.in_guest));
    let frozen = runs.load(Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(runs.load(Ordering::SeqCst), frozen, "a parked vcpu ran");
    control.resume().expect("resume");
    std::thread::sleep(Duration::from_millis(30));
    assert!(runs.load(Ordering::SeqCst) > frozen, "vcpus did not resume");
    control.request_stop(false).expect("stop");
    assert!(stopped.load(Ordering::SeqCst), "stop hook not called");
    for thread in threads {
        thread.join().expect("fake vcpu");
    }
}

#[test]
fn stop_while_paused_wakes_parked_vcpus() {
    let control = Arc::new(VmControl::with_clock("t", 1, fake_clock));
    let (kicked, stopped) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    control.mark_running();
    control.pause(Duration::from_millis(100)).expect("pause");
    let parked = {
        let control = Arc::clone(&control);
        std::thread::spawn(move || control.enter_guest(0))
    };
    std::thread::sleep(Duration::from_millis(20));
    control.bind(hooks(kicked, stopped));
    control.request_stop(true).expect("force-stop");
    assert!(
        !parked.join().expect("parked thread"),
        "parked vcpu should see the stop"
    );
    assert!(control.forced_for().is_some());
    assert_eq!(control.status().stop_cause, Some(StopCause::ForceStop));
}

#[test]
fn pause_times_out_on_a_vcpu_that_never_leaves() {
    let control = VmControl::with_clock("t", 2, fake_clock);
    control.mark_running();
    assert!(control.enter_guest(1));
    match control.pause(Duration::from_millis(20)) {
        Err(ControlError::PauseTimeout { cpus, .. }) => assert_eq!(cpus, vec![1]),
        other => panic!("expected a timeout, got {other:?}"),
    }
    assert_eq!(control.state(), VmState::Running);
    assert!(control.enter_guest(0), "the withdrawn pause must not park");
}

#[test]
fn resume_adds_the_paused_host_ticks() {
    let control = VmControl::with_clock("ticks", 1, fake_clock);
    control.mark_running();
    control.pause(Duration::from_millis(100)).expect("pause");
    FAKE_TICKS.fetch_add(24_000, Ordering::SeqCst);
    control.resume().expect("resume");
    assert_eq!(control.paused_ticks(), 24_000);
}

#[test]
fn stop_before_bind_is_forwarded_and_failures_are_kept() {
    let control = VmControl::with_clock("t", 1, fake_clock);
    control.request_stop(false).expect("stop while created");
    let (kicked, stopped) = (
        Arc::new(AtomicBool::new(false)),
        Arc::new(AtomicBool::new(false)),
    );
    control.bind(hooks(kicked, Arc::clone(&stopped)));
    assert!(
        stopped.load(Ordering::SeqCst),
        "bind must forward the earlier stop"
    );
    control.finish(Err("vm dtb: boom".to_string()));
    let status = control.status();
    assert_eq!(status.state, VmState::Failed);
    assert_eq!(status.failure.as_deref(), Some("vm dtb: boom"));
}

#[test]
fn guest_power_off_records_the_exit_and_stats_list_cpus() {
    let control = VmControl::with_clock("t", 2, fake_clock);
    control.attach_cpu_stats(1, VcpuStatsSource::detached());
    control.mark_running();
    control.mark_stopping(ExitReason::SystemOff);
    control.finish(Ok(ExitReason::SystemOff));
    let stats = control.stats();
    assert_eq!(stats.status.state, VmState::Stopped);
    assert_eq!(
        stats.status.stop_cause,
        Some(StopCause::Guest(ExitReason::SystemOff))
    );
    assert_eq!(stats.cpus.len(), 2);
    assert!(stats.cpus[0].stats.is_none() && stats.cpus[1].stats.is_some());
}
