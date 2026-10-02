//! virtio-net scenario: boot with a loopback NIC, ping the fake gateway, capture a pcap.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{
    last_lines, open_backend, NetStats, Pl011, Step, VirtioNet, DEFAULT_MAC, PCAP_ENV,
};
use ternvale_vmm::{ExitReason, Machine, MachineError};

use crate::common::{
    assets_root, banner_timeout, drive, init_logging, log_dir, restore_stdin, stdin_pipe,
    write_result,
};

const COMMAND: Duration = Duration::from_secs(30);

pub fn run() -> Result<(), String> {
    let root = assets_root().join("virtio-net");
    for name in ["Image", "initramfs.cpio"] {
        let path = root.join(name);
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-net-initramfs.sh)",
                path.display()
            ));
        }
    }
    let log_dir = log_dir();
    let guard = init_logging("boot-net", &log_dir)?;
    let pcap = log_dir.join("net.pcap");
    // SAFETY: the harness is single-threaded here; no other thread reads the environment yet.
    unsafe { std::env::set_var(PCAP_ENV, &pcap) };
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        pcap = %pcap.display(),
        scenario = "net",
        "boot harness logs"
    );

    let serial_log = log_dir.join("guest-serial.log");
    let vm = VmConfig {
        name: "boot-net".to_string(),
        cpus: 1,
        ram_mib: 256,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: String::new(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: None,
        nvram: None,
    };
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host = guard.log_path().to_path_buf();
    let feeder =
        std::thread::spawn(move || drive(&serial_for_feed, &mut input, &flag, &host, script()));

    let stats: Arc<Mutex<Option<Arc<NetStats>>>> = Arc::new(Mutex::new(None));
    let stats_slot = Arc::clone(&stats);
    let started = Instant::now();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let fail = |error: ternvale_devices::NetError| MachineError::Attach(error.to_string());
        let backend = open_backend("loopback").map_err(fail)?;
        let (base, size, device, net_stats) = VirtioNet::attach(
            0,
            DEFAULT_MAC,
            backend,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(0),
        )
        .map_err(fail)?;
        if let Ok(mut slot) = stats_slot.lock() {
            *slot = Some(net_stats);
        }
        Ok(vec![(base, size, device)])
    });
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(&log_dir, &exit, &script_result, &serial);
    let counters = stats
        .lock()
        .ok()
        .and_then(|slot| slot.as_ref().map(|s| s.snapshot()));
    if let Some([tx_packets, tx_bytes, tx_dropped, rx_packets, rx_bytes, rx_dropped]) = counters {
        tracing::info!(
            target: "ternvale::net",
            tx_packets,
            tx_bytes,
            tx_dropped,
            rx_packets,
            rx_bytes,
            rx_dropped,
            "virtio-net counters at exit"
        );
    }
    script_result?;
    match exit {
        Ok(ExitReason::SystemOff) => {
            tracing::info!(
                target: "ternvale::boot",
                total_ms = started.elapsed().as_millis() as u64,
                scenario = "net",
                pcap = %pcap.display(),
                "boot harness passed"
            );
            drop(guard);
            Ok(())
        }
        other => Err(format!(
            "exit={other:?}\nlog={}\n--- serial ---\n{}",
            guard.log_path().display(),
            last_lines(&serial, 40)
        )),
    }
}

// PL011 RX FIFO is 16 bytes; every Send below stays within that.
fn send(data: &[u8]) -> Step {
    Step::Send {
        data: data.to_vec(),
    }
}

fn expect(pattern: &str, timeout: Duration) -> Step {
    Step::Expect {
        pattern: pattern.to_string(),
        timeout,
    }
}

fn script() -> Vec<Step> {
    let banner = banner_timeout();
    vec![
        expect("Linux version", banner),
        expect("ternvale net ready mac=52:54:00:12:34:56", banner),
        expect("# ", banner),
        send(b"ip addr add "),
        send(b"10.0.2.15/24 "),
        send(b"dev eth0\n"),
        expect("# ", COMMAND),
        send(b"ip link set "),
        send(b"eth0 up\n"),
        expect("# ", COMMAND),
        send(b"ping -c 3 "),
        send(b"10.0.2.2\n"),
        // ", 0%" so that "100% packet loss" cannot match.
        expect("3 packets received, 0% packet loss", COMMAND),
        expect("# ", COMMAND),
        send(b"ip neigh\n"),
        expect("10.0.2.2 dev eth0 lladdr 52:55:0a:00:02:02", COMMAND),
        expect("# ", COMMAND),
        send(b"/bin/busybox "),
        send(b"poweroff -f\n"),
    ]
}
