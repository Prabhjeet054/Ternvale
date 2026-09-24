//! Run `guest-tests/hello.bin` and collect each `strb` byte until `hvc #0`.

use crate::boot::{load, PAYLOAD_GPA};
use crate::esr::{decode, ExitEvent};
use crate::memory::{GuestMemory, HOST_PAGE_SIZE};
use crate::platform::{GIC_DIST_BASE, GIC_REDIST_BASE};
use crate::vcpu::{ExitReason, Vcpu};

const UART: u64 = 0x0900_0000;
/// EL1h with DAIF masked. `M[3:0] = 0x5`, plus bits D, A, I, and F.
const CPSR_EL1H: u64 = 0x3c5;
const EXPECTED: &[u8] = b"Hello from Ternvale\n";

#[test]
#[ignore = "needs-hv"]
fn runs_hello_payload_and_collects_strb_bytes() {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-hello", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let previous = std::env::var("TERNVALE_LOG").ok();
    // SAFETY: this ignored test sets TERNVALE_LOG for the hello run and restores it.
    unsafe { std::env::set_var("TERNVALE_LOG", "trace") };
    let mut config = ternvale_log::LogConfig::new("hello", dir.clone());
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

    let mut captured = Vec::new();
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
        // `run` already decoded this syndrome for the log. Decode again to branch.
        match decode(syndrome, physical_address) {
            ExitEvent::Mmio {
                gpa,
                size,
                write,
                reg,
            } => {
                assert_eq!(gpa, UART, "strb gpa {gpa:#x}");
                assert!(write, "expected a write");
                assert_eq!(size, 1, "strb is one byte");
                let value = vcpu.get_x(reg).expect("source reg");
                let byte = value as u8;
                tracing::trace!(
                    target: "ternvale::mmio",
                    addr = format!("{:#x}", gpa),
                    size,
                    value = format!("{:#x}", byte),
                    direction = "write",
                    "mmio write"
                );
                captured.push(byte);
                // The faulting `strb` is re-executed unless PC moves past it.
                // TODO(verify): Apple's exit PC for a data abort is the faulting
                // instruction. HVC and SMC still follow the note in `esr`.
                let pc = vcpu.get_pc().expect("pc");
                vcpu.set_pc(pc + 4).expect("skip strb");
            }
            ExitEvent::Hvc { imm16 } => {
                assert_eq!(imm16, 0);
                saw_hvc = true;
                break;
            }
            other => panic!(
                "unexpected exit {other:?} syndrome={syndrome:#x} pc={:#x}",
                vcpu.get_pc().unwrap_or(0)
            ),
        }
    }

    tracing::info!(
        target: "ternvale::boot",
        bytes = %String::from_utf8_lossy(&captured),
        "captured guest bytes"
    );
    assert!(saw_hvc, "hvc #0 did not follow the stores");
    assert_eq!(captured, EXPECTED);

    drop(vcpu);
    drop(memory);
    drop(vm);
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    let mmio_lines = text.matches("mmio write addr=\"0x9000000\"").count();
    assert_eq!(mmio_lines, EXPECTED.len(), "{text}");
    assert!(text.contains("hvc imm16=0x0"), "{text}");
    assert!(text.contains("Hello from Ternvale"), "{text}");
    let sample = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../target/ternvale-log-samples/hello-payload.log");
    if let Some(parent) = sample.parent() {
        std::fs::create_dir_all(parent).expect("sample dir");
    }
    std::fs::write(&sample, &text).expect("sample");
    std::fs::remove_dir_all(&dir).expect("remove log dir");
    // SAFETY: same as the set above; this test restores the variable it changed.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TERNVALE_LOG", value),
            None => std::env::remove_var("TERNVALE_LOG"),
        }
    }
}
