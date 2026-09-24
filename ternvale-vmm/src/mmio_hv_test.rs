//! Run the hello payload and deliver each `strb` to a mock device on the bus.

use std::cell::RefCell;
use std::rc::Rc;

use crate::boot::{load, PAYLOAD_GPA};
use crate::esr::{decode, ExitEvent};
use crate::memory::{GuestMemory, HOST_PAGE_SIZE};
use crate::mmio::{MmioBus, MmioDevice};
use crate::platform::{GIC_DIST_BASE, GIC_REDIST_BASE};
use crate::vcpu::{ExitReason, Vcpu};

const UART: u64 = 0x0900_0000;
const UART_SIZE: u64 = 0x1000;
/// EL1h with DAIF masked. `M[3:0] = 0x5`, plus bits D, A, I, and F.
const CPSR_EL1H: u64 = 0x3c5;
const EXPECTED: &[u8] = b"Hello from Ternvale\n";

struct Sink {
    bytes: Rc<RefCell<Vec<u8>>>,
}

impl MmioDevice for Sink {
    fn name(&self) -> &str {
        "mock-uart"
    }

    fn read(&mut self, _offset: u64, _size: u8) -> u64 {
        0
    }

    fn write(&mut self, _offset: u64, _size: u8, val: u64) {
        self.bytes.borrow_mut().push(val as u8);
    }
}

#[test]
#[ignore = "needs-hv"]
fn runs_hello_payload_through_the_bus() {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-mmio-hello", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let previous = std::env::var("TERNVALE_LOG").ok();
    // SAFETY: this ignored test sets TERNVALE_LOG for the hello bus run and restores it.
    unsafe { std::env::set_var("TERNVALE_LOG", "trace") };
    let mut config = ternvale_log::LogConfig::new("mmiohello", dir.clone());
    config.level = "trace".to_string();
    let guard = ternvale_log::init(config).expect("log");

    let bin_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../guest-tests/hello.bin");
    let payload = std::fs::read(&bin_path).unwrap_or_else(|error| {
        panic!(
            "read {} (run `make guest-tests` first): {error}",
            bin_path.display()
        )
    });

    let vm = ternvale_hv::Vm::create().expect("vm");
    vm.create_gic(GIC_DIST_BASE, GIC_REDIST_BASE).expect("gic");
    let mut memory = GuestMemory::new().expect("memory");
    memory.map(&vm, PAYLOAD_GPA, HOST_PAGE_SIZE).expect("map");
    let vcpu = Vcpu::create(&vm).expect("vcpu");
    vcpu.set_cpsr(CPSR_EL1H).expect("cpsr");
    load(&mut memory, &vcpu, &payload).expect("load");

    let received = Rc::new(RefCell::new(Vec::new()));
    let mut bus = MmioBus::new();
    bus.register(
        UART,
        UART_SIZE,
        Box::new(Sink {
            bytes: Rc::clone(&received),
        }),
    )
    .expect("uart");

    let mut saw_hvc = false;
    for _ in 0..64 {
        let exit = vcpu.run().expect("run");
        let ExitReason::Exception {
            syndrome,
            physical_address,
            ..
        } = exit
        else {
            panic!("expected an exception exit, got {exit:?}");
        };
        let event = decode(syndrome, physical_address);
        match event {
            ExitEvent::Mmio { .. } => {
                bus.dispatch(&vcpu, event).expect("dispatch");
                // The faulting `strb` is re-executed unless PC moves past it.
                let pc = vcpu.get_pc().expect("pc");
                vcpu.set_pc(pc + 4).expect("skip strb");
            }
            ExitEvent::Hvc { imm16 } => {
                assert_eq!(imm16, 0);
                saw_hvc = true;
                break;
            }
            other => panic!("unexpected exit {other:?} syndrome={syndrome:#x}"),
        }
    }

    let bytes = received.borrow().clone();
    tracing::info!(
        target: "ternvale::mmio",
        bytes = %String::from_utf8_lossy(&bytes),
        "mock uart received"
    );
    assert!(saw_hvc, "hvc #0 did not follow the stores");
    assert_eq!(bytes, EXPECTED);
    assert_eq!(bus.access_count("mock-uart"), Some(EXPECTED.len() as u64));

    drop(bus);
    drop(vcpu);
    drop(memory);
    drop(vm);
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    assert!(text.contains("device=\"mock-uart\""), "{text}");
    assert!(text.contains("Hello from Ternvale"), "{text}");
    assert!(text.contains("accesses=20"), "{text}");
    std::fs::remove_dir_all(&dir).expect("remove log dir");
    // SAFETY: same as the set above; this test restores the variable it changed.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TERNVALE_LOG", value),
            None => std::env::remove_var("TERNVALE_LOG"),
        }
    }
}
