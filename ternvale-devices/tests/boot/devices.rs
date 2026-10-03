//! virtio-rng + virtio-vsock scenario: read /dev/hwrng, then ping/pong over vsock port 5000.

use std::io::{ErrorKind, Read, Write};
use std::os::unix::net::UnixListener;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ternvale_config::VmConfig;
use ternvale_devices::{
    host_socket_path, last_lines, Pl011, RngStats, Step, VirtioRng, VirtioVsock, VsockConfig,
    VsockStats,
};
use ternvale_vmm::{ExitReason, Machine, MachineError};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging_at, log_dir, restore_stdin,
    stdin_pipe, write_result,
};

const COMMAND: Duration = Duration::from_secs(30);
const GUEST_CID: u32 = 3;
const PORT: u32 = 5000;
const LOG_FILTER: &str = "info,ternvale::virtio::vsock=trace,ternvale::virtio::rng=trace";

type DeviceStats = Arc<Mutex<Option<(Arc<RngStats>, Arc<VsockStats>)>>>;

pub fn run() -> Result<(), String> {
    let root = assets_root().join("virtio-devices");
    for name in ["Image", "initramfs.cpio"] {
        let path = root.join(name);
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-devices-initramfs.sh)",
                path.display()
            ));
        }
    }
    let log_dir = log_dir();
    let guard = init_logging_at("boot-devices", &log_dir, LOG_FILTER)?;
    // macOS caps socket paths at 104 bytes, so the sockets live under the short temp dir.
    let uds_dir = std::env::temp_dir().join(format!("tv-{}", std::process::id()));
    std::fs::create_dir_all(&uds_dir).map_err(|err| format!("create uds dir: {err}"))?;
    let host_sock = host_socket_path(&uds_dir, PORT);
    let listener = UnixListener::bind(&host_sock)
        .map_err(|err| format!("bind {}: {err}", host_sock.display()))?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        uds = %host_sock.display(),
        scenario = "devices",
        "boot harness logs"
    );

    let serial_log = log_dir.join("guest-serial.log");
    let cpus = boot_cpus(1)?;
    let vm = VmConfig {
        name: "boot-devices".to_string(),
        cpus,
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
        firmware_tables: None,
        vsock: None,
    };
    let uart = Pl011::open(&serial_log).map_err(|err| err.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let server = host_server(listener, Arc::clone(&cancel));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host = guard.log_path().to_path_buf();
    let feeder =
        std::thread::spawn(move || drive(&serial_for_feed, &mut input, &flag, &host, script()));

    let stats: DeviceStats = Arc::new(Mutex::new(None));
    let stats_slot = Arc::clone(&stats);
    let vsock_config = VsockConfig {
        guest_cid: Some(GUEST_CID),
        uds_dir: uds_dir.clone(),
        listen_ports: Vec::new(),
    };
    let started = Instant::now();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let (rng_base, rng_size, rng, rng_stats) =
            VirtioRng::attach(0, Arc::clone(&attach.memory), attach.virtio_irq_hook(0))
                .map_err(|error| MachineError::Attach(error.to_string()))?;
        let (vs_base, vs_size, vsock, vsock_stats, cid) = VirtioVsock::attach(
            1,
            &vsock_config,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(1),
        )
        .map_err(|error| MachineError::Attach(error.to_string()))?;
        tracing::info!(target: "ternvale::boot", guest_cid = cid, "attached virtio-rng and virtio-vsock");
        if let Ok(mut slot) = stats_slot.lock() {
            *slot = Some((rng_stats, vsock_stats));
        }
        Ok(vec![(rng_base, rng_size, rng), (vs_base, vs_size, vsock)])
    });
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let host_result = server
        .join()
        .unwrap_or_else(|_| Err("host server panicked".to_string()));
    cleanup(&uds_dir);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(&log_dir, &exit, &script_result, &serial);
    log_stats(&stats);
    script_result?;
    let received = host_result?;
    if received != b"ping" {
        return Err(format!("host received {received:?}, expected \"ping\""));
    }
    check_hwrng(&serial)?;
    match exit {
        Ok(ExitReason::SystemOff) => {
            tracing::info!(
                target: "ternvale::boot",
                total_ms = started.elapsed().as_millis() as u64,
                scenario = "devices",
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

/// Accept one guest connection on `host-5000.sock`, require "ping", answer "pong",
/// then wait for the guest to close. Returns the bytes received.
fn host_server(
    listener: UnixListener,
    cancel: Arc<AtomicBool>,
) -> JoinHandle<Result<Vec<u8>, String>> {
    std::thread::spawn(move || {
        listener
            .set_nonblocking(true)
            .map_err(|err| format!("listener nonblocking: {err}"))?;
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    if cancel.load(Ordering::Acquire) {
                        return Err("guest never connected to vsock port 5000".to_string());
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => return Err(format!("accept: {e}")),
            }
        };
        tracing::info!(target: "ternvale::boot", port = PORT, "vsock host side accepted guest connection");
        stream
            .set_nonblocking(false)
            .and_then(|()| stream.set_read_timeout(Some(Duration::from_secs(10))))
            .map_err(|err| format!("stream setup: {err}"))?;
        let mut got = [0u8; 4];
        stream
            .read_exact(&mut got)
            .map_err(|err| format!("host read: {err}"))?;
        tracing::info!(
            target: "ternvale::boot",
            received = %String::from_utf8_lossy(&got),
            "vsock host side received"
        );
        stream
            .write_all(b"pong")
            .map_err(|err| format!("host write: {err}"))?;
        tracing::info!(target: "ternvale::boot", sent = "pong", "vsock host side replied");
        let mut rest = [0u8; 16];
        match stream.read(&mut rest) {
            Ok(0) => tracing::info!(target: "ternvale::boot", "guest closed the vsock stream"),
            Ok(n) => {
                tracing::warn!(target: "ternvale::boot", extra = n, "unexpected bytes after ping")
            }
            Err(error) => {
                tracing::warn!(target: "ternvale::boot", error = %error, "no close from guest")
            }
        }
        Ok(got.to_vec())
    })
}

/// Every `xxd` line must have 32 hex digits, not all zero, and samples must differ.
fn check_hwrng(serial: &str) -> Result<(), String> {
    let samples: Vec<String> = serial
        .lines()
        .filter_map(|line| line.find("00000000: ").map(|i| &line[i + 10..]))
        .map(|rest| {
            rest.chars()
                .take(39)
                .filter(|c| !c.is_whitespace())
                .collect()
        })
        .collect();
    tracing::info!(target: "ternvale::boot", samples = ?samples, "hwrng samples");
    if samples.len() < 2 {
        return Err(format!("expected 2 xxd samples, found {}", samples.len()));
    }
    for s in &samples {
        if s.len() != 32 || !s.chars().all(|c| c.is_ascii_hexdigit()) {
            return Err(format!("malformed xxd sample {s:?}"));
        }
        if s.chars().all(|c| c == '0') {
            return Err("hwrng returned 16 zero bytes".to_string());
        }
    }
    if samples[0] == samples[1] {
        return Err(format!("two hwrng reads were identical: {}", samples[0]));
    }
    Ok(())
}

fn log_stats(stats: &DeviceStats) {
    let Some((rng, vsock)) = stats.lock().ok().and_then(|slot| slot.clone()) else {
        return;
    };
    let [requests, bytes, errors] = rng.snapshot();
    tracing::info!(target: "ternvale::boot", requests, bytes, errors, "virtio-rng counters at exit");
    let v = vsock.snapshot();
    tracing::info!(
        target: "ternvale::boot",
        tx_packets = v.tx_packets,
        tx_bytes = v.tx_bytes,
        rx_packets = v.rx_packets,
        rx_bytes = v.rx_bytes,
        connections = v.connections,
        resets = v.resets,
        dropped = v.dropped,
        "virtio-vsock counters at exit"
    );
}

fn cleanup(dir: &Path) {
    if let Err(error) = std::fs::remove_dir_all(dir) {
        tracing::warn!(target: "ternvale::boot", dir = %dir.display(), error = %error, "could not remove uds dir");
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

fn hwrng_sample() -> [Step; 5] {
    [
        send(b"cat /dev/hwrng "),
        send(b"| head -c 16 "),
        send(b"| xxd\n"),
        expect("00000000: ", COMMAND),
        expect("# ", COMMAND),
    ]
}

fn script() -> Vec<Step> {
    let banner = banner_timeout();
    let mut steps = vec![
        expect("Linux version", banner),
        expect("ternvale devices ready rng=virtio_rng", banner),
        expect("vsock=yes", banner),
        expect("# ", banner),
    ];
    steps.extend(hwrng_sample());
    steps.extend(hwrng_sample());
    steps.extend([
        send(b"vsock-ping "),
        send(b"5000\n"),
        expect("vsock-ping: local cid 3", COMMAND),
        expect("vsock-ping: connected to cid 2 port 5000", COMMAND),
        expect("vsock-ping: received pong", COMMAND),
        expect("# ", COMMAND),
        send(b"echo rc=$?\n"),
        expect("rc=0", COMMAND),
        expect("# ", COMMAND),
        send(b"/bin/busybox "),
        send(b"poweroff -f\n"),
    ]);
    steps
}
