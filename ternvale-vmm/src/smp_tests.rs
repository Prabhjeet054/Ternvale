use std::sync::Arc;
use std::time::Duration;

use super::{dt_cpu_reg, gicr_affinity, mpidr, CpuPower, MPIDR_RES1};
use crate::psci::{
    sign, PowerRequest, AFFINITY_OFF, AFFINITY_ON, AFFINITY_ON_PENDING, ALREADY_ON,
    INVALID_ADDRESS, INVALID_PARAMS, ON_PENDING, SUCCESS,
};
use crate::vcpu::ExitReason;

const RAM: u64 = 0x4000_0000;
const RAM_SIZE: u64 = 0x1000_0000;

fn on(target: u64, entry: u64, context: u64) -> PowerRequest {
    PowerRequest::CpuOn {
        target,
        entry,
        context,
    }
}

fn info(target: u64) -> PowerRequest {
    PowerRequest::AffinityInfo { target, level: 0 }
}

#[test]
fn mpidr_uses_sixteen_cpus_per_aff1_cluster() {
    assert_eq!(mpidr(0), MPIDR_RES1);
    assert_eq!(mpidr(1), 0x8000_0001);
    assert_eq!(mpidr(15), 0x8000_000f);
    assert_eq!(mpidr(16), 0x8000_0100);
    assert_eq!(mpidr(17), 0x8000_0101);
    assert_eq!(dt_cpu_reg(3), 3);
    assert_eq!(dt_cpu_reg(17), 0x101);
    assert_eq!(gicr_affinity(mpidr(17)), 0x101);
    assert_eq!(gicr_affinity(0x12_8003_0405), 0x1203_0405);
}

#[test]
fn index_of_rejects_unknown_affinity_and_non_affinity_bits() {
    let power = CpuPower::new(4, RAM, RAM_SIZE);
    assert_eq!(power.index_of(0), Some(0));
    assert_eq!(power.index_of(3), Some(3));
    assert_eq!(power.index_of(4), None);
    assert_eq!(power.index_of(MPIDR_RES1 | 1), None, "RES1 is not affinity");
}

#[test]
fn cpu_on_returns_each_psci_status() {
    let power = CpuPower::new(2, RAM, RAM_SIZE);
    assert_eq!(power.handle(0, on(0, RAM, 0)), sign(ALREADY_ON));
    assert_eq!(power.handle(0, on(7, RAM, 0)), sign(INVALID_PARAMS));
    assert_eq!(power.handle(0, on(1, RAM + 2, 0)), sign(INVALID_ADDRESS));
    assert_eq!(power.handle(0, on(1, RAM - 4, 0)), sign(INVALID_ADDRESS));
    assert_eq!(
        power.handle(0, on(1, RAM + RAM_SIZE, 0)),
        sign(INVALID_ADDRESS)
    );
    assert_eq!(power.handle(0, info(1)), sign(AFFINITY_OFF));
    assert_eq!(power.handle(0, on(1, RAM + 0x1000, 9)), sign(SUCCESS));
    assert_eq!(power.handle(0, info(1)), sign(AFFINITY_ON_PENDING));
    assert_eq!(power.handle(0, on(1, RAM + 0x1000, 9)), sign(ON_PENDING));
    assert_eq!(power.wait_for_on(1), Some((RAM + 0x1000, 9)));
    assert_eq!(power.handle(0, info(1)), sign(AFFINITY_ON));
    assert_eq!(power.handle(0, on(1, RAM, 0)), sign(ALREADY_ON));
    assert_eq!(
        power.handle(
            0,
            PowerRequest::AffinityInfo {
                target: 1,
                level: 1
            }
        ),
        sign(INVALID_PARAMS)
    );
}

#[test]
fn a_waiting_secondary_starts_with_the_entry_and_context_from_cpu_on() {
    let power = Arc::new(CpuPower::new(2, RAM, RAM_SIZE));
    let waiter = Arc::clone(&power);
    let secondary = std::thread::spawn(move || waiter.wait_for_on(1));
    std::thread::sleep(Duration::from_millis(20));
    assert_eq!(
        power.handle(0, on(1, RAM + 0x8_0000, 0xfeed)),
        sign(SUCCESS)
    );
    let started = secondary.join().expect("secondary thread");
    assert_eq!(started, Some((RAM + 0x8_0000, 0xfeed)));
}

#[test]
fn request_stop_wakes_every_waiter_and_keeps_the_first_reason() {
    let power = Arc::new(CpuPower::new(3, RAM, RAM_SIZE));
    let off = Arc::clone(&power);
    let ready = Arc::clone(&power);
    let waiting_on = std::thread::spawn(move || off.wait_for_on(2));
    let waiting_ready = std::thread::spawn(move || ready.wait_all_ready());
    std::thread::sleep(Duration::from_millis(20));
    power.request_stop(ExitReason::SystemOff);
    power.request_stop(ExitReason::Canceled);
    assert_eq!(waiting_on.join().expect("on waiter"), None);
    assert!(!waiting_ready.join().expect("ready waiter"));
    assert_eq!(power.stop_reason(), Some(ExitReason::SystemOff));
}

#[test]
fn wait_all_ready_returns_once_every_cpu_is_ready() {
    let power = Arc::new(CpuPower::new(2, RAM, RAM_SIZE));
    let waiter = Arc::clone(&power);
    let boot = std::thread::spawn(move || waiter.wait_all_ready());
    power.mark_ready(0);
    std::thread::sleep(Duration::from_millis(20));
    assert!(!boot.is_finished(), "cpu 1 is not ready yet");
    power.mark_ready(1);
    assert!(boot.join().expect("boot waiter"));
}

#[test]
fn vcpus_are_created_in_cpu_index_order() {
    let power = Arc::new(CpuPower::new(3, RAM, RAM_SIZE));
    let order = Arc::new(std::sync::Mutex::new(Vec::new()));
    let threads: Vec<_> = (0..3u32)
        .rev()
        .map(|index| {
            let (power, order) = (Arc::clone(&power), Arc::clone(&order));
            std::thread::spawn(move || {
                assert!(power.wait_create_turn(index));
                order.lock().expect("order").push(index);
                power.mark_created(index);
            })
        })
        .collect();
    for thread in threads {
        thread.join().expect("creator");
    }
    assert_eq!(*order.lock().expect("order"), vec![0, 1, 2]);
    let late = CpuPower::new(2, RAM, RAM_SIZE);
    late.request_stop(ExitReason::Canceled);
    assert!(
        !late.wait_create_turn(1),
        "a stop releases waiting creators"
    );
}

#[test]
fn cpu_off_of_the_last_cpu_stops_the_vm() {
    let power = CpuPower::new(2, RAM, RAM_SIZE);
    assert_eq!(power.handle(0, on(1, RAM, 0)), sign(SUCCESS));
    assert!(power.wait_for_on(1).is_some());
    power.cpu_off(1);
    assert_eq!(power.handle(0, info(1)), sign(AFFINITY_OFF));
    assert_eq!(power.stop_reason(), None);
    power.cpu_off(0);
    assert_eq!(power.stop_reason(), Some(ExitReason::CpuOff));
    assert!(
        !power.register(0, crate::vcpu::VcpuStop::detached(u64::MAX)),
        "no vcpu may join a stopping vm"
    );
}
