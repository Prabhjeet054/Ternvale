//! Loom model of the Step 25 lock order in `docs/ARCHITECTURE.md`.
//!
//! Run with `./scripts/loom.sh` (`RUSTFLAGS=--cfg loom`). The real VMM uses
//! `lockwatch` (std mutexes); this file only checks that the documented
//! acquire order does not deadlock under permutation.

#[cfg(not(loom))]
#[test]
fn run_scripts_loom_sh_for_the_step25_model() {
    // Normal `cargo test` does not permute threads. `./scripts/loom.sh` does.
}

#[cfg(loom)]
mod model {
    use loom::sync::{Arc, Mutex};
    use loom::thread;

    struct Order {
        mmio_slot: Mutex<()>,
        serial: Mutex<()>,
        guest_memory: Mutex<()>,
        virtio_irq: Mutex<()>,
        pci_intx_pin: Mutex<()>,
        pci_intx_line: Mutex<()>,
    }

    impl Order {
        fn new() -> Arc<Self> {
            Arc::new(Self {
                mmio_slot: Mutex::new(()),
                serial: Mutex::new(()),
                guest_memory: Mutex::new(()),
                virtio_irq: Mutex::new(()),
                pci_intx_pin: Mutex::new(()),
                pci_intx_line: Mutex::new(()),
            })
        }
    }

    #[test]
    fn vcpu_mmio_then_memory_with_worker_on_memory() {
        loom::model(|| {
            let order = Order::new();
            let worker = {
                let order = Arc::clone(&order);
                thread::spawn(move || {
                    let _mem = order.guest_memory.lock().unwrap();
                })
            };
            {
                let _slot = order.mmio_slot.lock().unwrap();
                let _serial = order.serial.lock().unwrap();
                let _mem = order.guest_memory.lock().unwrap();
            }
            worker.join().unwrap();
        });
    }

    #[test]
    fn irq_then_pci_intx_after_dropping_memory() {
        loom::model(|| {
            let order = Order::new();
            {
                let _slot = order.mmio_slot.lock().unwrap();
                let _mem = order.guest_memory.lock().unwrap();
            }
            {
                let _irq = order.virtio_irq.lock().unwrap();
            }
            {
                let _pin = order.pci_intx_pin.lock().unwrap();
                let _line = order.pci_intx_line.lock().unwrap();
            }
        });
    }

    #[test]
    fn two_workers_raise_intx_without_earlier_locks() {
        loom::model(|| {
            let order = Order::new();
            let a = {
                let order = Arc::clone(&order);
                thread::spawn(move || {
                    let _pin = order.pci_intx_pin.lock().unwrap();
                    let _line = order.pci_intx_line.lock().unwrap();
                })
            };
            let b = {
                let order = Arc::clone(&order);
                thread::spawn(move || {
                    let _pin = order.pci_intx_pin.lock().unwrap();
                    let _line = order.pci_intx_line.lock().unwrap();
                })
            };
            a.join().unwrap();
            b.join().unwrap();
        });
    }
}
