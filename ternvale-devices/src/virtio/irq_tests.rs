use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Barrier};

use super::VirtioIrq;

fn recording() -> (Arc<VirtioIrq>, Arc<AtomicBool>, Arc<AtomicU64>) {
    let irq = VirtioIrq::new();
    let level = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicU64::new(0));
    let (l, c) = (Arc::clone(&level), Arc::clone(&calls));
    irq.set_hook(Arc::new(move |up| {
        // As slow as the real hook (hv_gic_set_spi), which is what opens the race.
        let until = std::time::Instant::now() + std::time::Duration::from_micros(2);
        while std::time::Instant::now() < until {
            std::hint::spin_loop();
        }
        l.store(up, Ordering::SeqCst);
        c.fetch_add(1, Ordering::SeqCst);
    }));
    (irq, level, calls)
}

#[test]
fn raise_and_ack_drive_the_line() {
    let (irq, level, calls) = recording();
    irq.raise(1);
    assert!(level.load(Ordering::SeqCst));
    irq.raise(2);
    assert_eq!(irq.status(), 3);
    irq.ack(1);
    assert!(level.load(Ordering::SeqCst), "bit 2 still pending");
    irq.ack(2);
    assert!(!level.load(Ordering::SeqCst));
    assert_eq!(calls.load(Ordering::SeqCst), 2, "only edges reach the hook");
    irq.ack(3);
    irq.raise(0);
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

/// The exact Step 65 interleaving, forced: the vCPU's ack clears the status and
/// starts lowering the line; before the lower lands, the device raises again.
/// The line must end up high, because the interrupt is pending.
#[test]
fn raise_during_ack_leaves_the_line_high() {
    let irq = VirtioIrq::new();
    let level = Arc::new(AtomicBool::new(false));
    let lowering = Arc::new(AtomicBool::new(false));
    let raised = Arc::new(AtomicBool::new(false));
    {
        let (level, lowering, raised) = (
            Arc::clone(&level),
            Arc::clone(&lowering),
            Arc::clone(&raised),
        );
        irq.set_hook(Arc::new(move |up| {
            if !up {
                lowering.store(true, Ordering::SeqCst);
                // Hold the lower until the device's raise is done, or 200 ms when
                // the raise cannot finish first (it waits for this lower).
                let until = std::time::Instant::now() + std::time::Duration::from_millis(200);
                while !raised.load(Ordering::SeqCst) && std::time::Instant::now() < until {
                    std::thread::yield_now();
                }
            }
            level.store(up, Ordering::SeqCst);
        }));
    }
    irq.raise(1);
    let device = {
        let (irq, lowering, raised) =
            (Arc::clone(&irq), Arc::clone(&lowering), Arc::clone(&raised));
        std::thread::spawn(move || {
            while !lowering.load(Ordering::SeqCst) {
                std::thread::yield_now();
            }
            irq.raise(1);
            raised.store(true, Ordering::SeqCst);
        })
    };
    irq.ack(1);
    device.join().expect("device thread");
    assert_eq!(irq.status(), 1);
    assert!(
        level.load(Ordering::SeqCst),
        "interrupt pending but the line is low"
    );
}

/// A device thread raising while the vCPU acks must never leave the line low with
/// status pending (the guest would never see that interrupt): the Step 65 stall.
#[test]
fn concurrent_raise_and_ack_never_lose_the_line() {
    for round in 0..50 {
        let (irq, level, _) = recording();
        let start = Arc::new(Barrier::new(2));
        let device = {
            let (irq, start) = (Arc::clone(&irq), Arc::clone(&start));
            std::thread::spawn(move || {
                start.wait();
                for _ in 0..20_000 {
                    irq.raise(1);
                }
            })
        };
        start.wait();
        for _ in 0..20_000 {
            irq.ack(1);
        }
        device.join().expect("device thread");
        assert_eq!(
            level.load(Ordering::SeqCst),
            irq.status() != 0,
            "round {round}: line does not match status {:#x}",
            irq.status()
        );
    }
}
