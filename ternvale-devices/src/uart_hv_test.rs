//! Run the hello payload through the real PL011 and check stdout plus the serial log.

use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use ternvale_hv::Vm;
use ternvale_vmm::{
    decode_esr, load_payload, ExitEvent, ExitReason, GuestMemory, MmioBus, Vcpu, GIC_DIST_BASE,
    GIC_REDIST_BASE, HOST_PAGE_SIZE, PAYLOAD_GPA,
};

use super::{Pl011, PL011_BASE, PL011_SIZE};

const CPSR_EL1H: u64 = 0x3c5;
const EXPECTED: &[u8] = b"Hello from Ternvale\n";

struct StdoutPipe {
    saved: i32,
    read_fd: i32,
}

impl StdoutPipe {
    fn start() -> Self {
        let mut ends = [0; 2];
        // SAFETY: `pipe` writes two open fds into `ends` on success.
        let rc = unsafe { libc::pipe(ends.as_mut_ptr()) };
        assert_eq!(rc, 0, "pipe");
        // SAFETY: `dup` duplicates stdout. `dup2` makes stdout the pipe's write end.
        let saved = unsafe { libc::dup(libc::STDOUT_FILENO) };
        assert!(saved >= 0, "dup stdout");
        let rc = unsafe { libc::dup2(ends[1], libc::STDOUT_FILENO) };
        assert_eq!(rc, libc::STDOUT_FILENO, "dup2");
        // SAFETY: stdout now holds the write end, so the original can close.
        unsafe { libc::close(ends[1]) };
        Self {
            saved,
            read_fd: ends[0],
        }
    }

    fn finish(self) -> Vec<u8> {
        let _ = std::io::stdout().flush();
        // SAFETY: restore the saved stdout, then close that duplicate.
        unsafe {
            libc::dup2(self.saved, libc::STDOUT_FILENO);
            libc::close(self.saved);
        }
        // SAFETY: `read_fd` is the pipe read end this guard owns.
        let mut file = unsafe { std::fs::File::from_raw_fd(self.read_fd) };
        let mut buf = Vec::new();
        file.read_to_end(&mut buf).expect("read captured stdout");
        buf
    }
}

#[test]
#[ignore = "needs-hv"]
fn runs_hello_payload_on_the_pl011() {
    let dir = std::env::temp_dir().join(format!("ternvale-uart-{}-hello", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let serial_log = dir.join("guest-serial.log");
    let previous = std::env::var("TERNVALE_LOG").ok();
    // SAFETY: this ignored test sets TERNVALE_LOG and restores it before returning.
    unsafe { std::env::set_var("TERNVALE_LOG", "debug") };
    let mut config = ternvale_log::LogConfig::new("uarthi", dir.clone());
    config.level = "debug".to_string();
    let guard = ternvale_log::init(config).expect("log");

    let bin_path =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../guest-tests/hello.bin");
    let payload = std::fs::read(&bin_path).unwrap_or_else(|error| {
        panic!(
            "read {} (run `make guest-tests` first): {error}",
            bin_path.display()
        )
    });

    let capture = StdoutPipe::start();
    let vm = Vm::create().expect("vm");
    vm.create_gic(GIC_DIST_BASE, GIC_REDIST_BASE).expect("gic");
    let mut memory = GuestMemory::new().expect("memory");
    memory.map(&vm, PAYLOAD_GPA, HOST_PAGE_SIZE).expect("map");
    let vcpu = Vcpu::create(&vm).expect("vcpu");
    vcpu.set_cpsr(CPSR_EL1H).expect("cpsr");
    load_payload(&mut memory, &vcpu, &payload).expect("load");

    let mut bus = MmioBus::new();
    bus.register(
        PL011_BASE,
        PL011_SIZE,
        Box::new(Pl011::open(&serial_log).expect("uart")),
    )
    .expect("register");

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
        let event = decode_esr(syndrome, physical_address);
        match event {
            ExitEvent::Mmio { .. } => {
                bus.dispatch(&vcpu, event).expect("dispatch");
                let pc = vcpu.get_pc().expect("pc");
                vcpu.set_pc(pc + 4).expect("skip strb");
            }
            ExitEvent::Hvc { imm16 } => {
                assert_eq!(imm16, 0);
                saw_hvc = true;
                break;
            }
            other => panic!("unexpected exit {other:?}"),
        }
    }
    assert!(saw_hvc, "hvc #0 did not follow the stores");
    drop(bus);
    drop(vcpu);
    drop(memory);
    drop(vm);

    let stdout = capture.finish();
    let serial = std::fs::read(&serial_log).expect("serial log");
    assert_eq!(stdout, EXPECTED);
    assert_eq!(serial, EXPECTED);
    std::io::stdout().write_all(&stdout).expect("replay stdout");
    let _ = std::io::stdout().flush();

    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("host log");
    assert!(text.contains("uart tx"), "{text}");
    assert!(text.contains("line=Hello from Ternvale"), "{text}");
    std::fs::remove_dir_all(&dir).expect("remove dir");
    // SAFETY: same as the set above; this test restores the variable it changed.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TERNVALE_LOG", value),
            None => std::env::remove_var("TERNVALE_LOG"),
        }
    }
}

const PSCI_VERSION_TEXT: &[u8] = b"00010001\n";

#[test]
#[ignore = "needs-hv"]
fn runs_psci_version_over_uart_then_system_off() {
    let dir = std::env::temp_dir().join(format!("ternvale-uart-{}-psci", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let serial_log = dir.join("guest-serial.log");
    let previous = std::env::var("TERNVALE_LOG").ok();
    // SAFETY: this ignored test sets TERNVALE_LOG and restores it before returning.
    unsafe { std::env::set_var("TERNVALE_LOG", "debug") };
    let mut config = ternvale_log::LogConfig::new("psci", dir.clone());
    config.level = "debug".to_string();
    let guard = ternvale_log::init(config).expect("log");

    let bin_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../guest-tests/psci.bin");
    let payload = std::fs::read(&bin_path).unwrap_or_else(|error| {
        panic!(
            "read {} (run `make guest-tests` first): {error}",
            bin_path.display()
        )
    });

    let capture = StdoutPipe::start();
    let vm = Vm::create().expect("vm");
    vm.create_gic(GIC_DIST_BASE, GIC_REDIST_BASE).expect("gic");
    let mut memory = GuestMemory::new().expect("memory");
    memory.map(&vm, PAYLOAD_GPA, HOST_PAGE_SIZE).expect("map");
    let vcpu = Vcpu::create(&vm).expect("vcpu");
    vcpu.set_cpsr(CPSR_EL1H).expect("cpsr");
    load_payload(&mut memory, &vcpu, &payload).expect("load");

    let mut bus = MmioBus::new();
    bus.register(
        PL011_BASE,
        PL011_SIZE,
        Box::new(Pl011::open(&serial_log).expect("uart")),
    )
    .expect("register");

    let done = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&done);
    let stopper = vcpu.stopper();
    let watchdog = std::thread::spawn(move || {
        for _ in 0..40 {
            if flag.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        if !flag.load(Ordering::Acquire) {
            stopper.request().expect("cancel a spinning guest");
        }
    });

    let mut exit = None;
    for _ in 0..64 {
        let reason = vcpu.run().expect("run");
        match reason {
            ExitReason::Exception {
                syndrome,
                physical_address,
                ..
            } => {
                let event = decode_esr(syndrome, physical_address);
                match event {
                    ExitEvent::Mmio { .. } => {
                        bus.dispatch(&vcpu, event).expect("dispatch");
                        let pc = vcpu.get_pc().expect("pc");
                        vcpu.set_pc(pc + 4).expect("skip strb");
                    }
                    other => panic!("unexpected exit {other:?}"),
                }
            }
            ExitReason::SystemOff => {
                exit = Some(reason);
                break;
            }
            other => panic!("expected SYSTEM_OFF, got {other:?}"),
        }
    }
    done.store(true, Ordering::Release);
    watchdog.join().expect("watchdog");
    assert_eq!(exit, Some(ExitReason::SystemOff));

    drop(bus);
    drop(vcpu);
    drop(memory);
    drop(vm);

    let stdout = capture.finish();
    let serial = std::fs::read(&serial_log).expect("serial log");
    assert_eq!(stdout, PSCI_VERSION_TEXT);
    assert_eq!(serial, PSCI_VERSION_TEXT);
    std::io::stdout().write_all(&stdout).expect("replay stdout");
    std::io::stdout().flush().expect("flush stdout");

    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("host log");
    assert!(
        text.contains("function=\"0x84000000\"") && text.contains("ret=\"0x10001\""),
        "{text}"
    );
    assert!(text.contains("system off"), "{text}");
    std::fs::remove_dir_all(&dir).expect("remove dir");
    // SAFETY: same as the set above; this test restores the variable it changed.
    unsafe {
        match previous {
            Some(value) => std::env::set_var("TERNVALE_LOG", value),
            None => std::env::remove_var("TERNVALE_LOG"),
        }
    }
}
