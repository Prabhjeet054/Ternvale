//! Clipboard (both directions) and virtio-9p share with a 100 MiB host→guest file.

use std::io::Write;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_agent_proto::{ClipboardText, Message, AGENT_PORT};
use ternvale_config::VmConfig;
use ternvale_devices::{
    host_socket_path, last_lines, AgentServer, AgentServerConfig, AgentState, Pl011, Step,
    VirtioP9, VirtioVsock, VsockConfig,
};
use ternvale_vmm::{ExitReason, Machine, MachineError};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging_at, log_dir, restore_stdin,
    stdin_pipe, write_result,
};

const GUEST_CID: u32 = 3;
const HOST_CLIP: &str = "host-copy-paste-test";
const GUEST_CLIP: &str = "guest-copy-paste-test";
const LARGE_BYTES: u64 = 100 * 1024 * 1024;
const CHUNK: usize = 64 * 1024;
const LOG_FILTER: &str =
    "info,ternvale::agent=debug,ternvale::virtio::p9=debug,ternvale::virtio::vsock=debug,ternvale::boot=debug";

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
    let guard = init_logging_at("boot-share-clipboard", &log_dir, LOG_FILTER)?;
    let share_host = std::env::temp_dir().join(format!("tvshare-{}", std::process::id()));
    std::fs::create_dir_all(&share_host).map_err(|e| format!("share dir: {e}"))?;
    let large_path = share_host.join("from_host.bin");
    let host_sha = write_large_file(&large_path)?;
    tracing::info!(
        target: "ternvale::boot",
        path = %large_path.display(),
        bytes = LARGE_BYTES,
        sha256 = %host_sha,
        "prepared large host share file"
    );

    let uds_dir = std::env::temp_dir().join(format!("tvsc-{}", std::process::id()));
    std::fs::create_dir_all(&uds_dir).map_err(|e| format!("uds dir: {e}"))?;
    let agent_sock = host_socket_path(&uds_dir, AGENT_PORT);
    let shares = vec![("ternvale".to_string(), "/mnt/ternvale".to_string())];
    let server = AgentServer::start_with(
        &agent_sock,
        "boot-share-clipboard",
        AgentServerConfig::default(),
        shares,
    )
    .map_err(|e| e.to_string())?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        share = %share_host.display(),
        scenario = "share-clipboard",
        "boot harness logs"
    );

    let serial_log = log_dir.join("guest-serial.log");
    let vm = VmConfig {
        name: "boot-share-clipboard".to_string(),
        profile: Default::default(),
        iso: None,
        cpus: boot_cpus(1)?,
        ram_mib: 512,
        kernel: root.join("Image"),
        initrd: Some(root.join("initramfs.cpio")),
        cmdline: "ternvale.agent=debug ternvale.sharetest=1".to_string(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: None,
        nvram: None,
        firmware_tables: None,
        vsock: None,
        tpm: None,
        usb: None,
        audio: None,
        shares: Vec::new(),
    };
    let uart = Pl011::open(&serial_log).map_err(|e| e.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host_log = guard.log_path().to_path_buf();
    let feeder =
        std::thread::spawn(move || drive(&serial_for_feed, &mut input, &flag, &host_log, script()));
    let sock = agent_sock.clone();
    let share_for_ctl = share_host.clone();
    let expected_host_sha = host_sha.clone();
    let serial_for_control = serial_log.clone();
    let controller = std::thread::spawn(move || {
        control(
            server,
            &sock,
            &serial_for_control,
            &share_for_ctl,
            &expected_host_sha,
        )
    });

    let vsock_config = VsockConfig {
        guest_cid: Some(GUEST_CID),
        uds_dir: uds_dir.clone(),
        listen_ports: Vec::new(),
    };
    let export = share_host.clone();
    let started = Instant::now();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let (vs_base, vs_size, vsock, _stats, cid) = VirtioVsock::attach(
            0,
            &vsock_config,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(0),
        )
        .map_err(|e| MachineError::Attach(e.to_string()))?;
        let (p9_base, p9_size, p9, _p9_stats) = VirtioP9::attach(
            1,
            export,
            "ternvale",
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(1),
        )
        .map_err(|e| MachineError::Attach(e.to_string()))?;
        tracing::info!(
            target: "ternvale::boot",
            guest_cid = cid,
            tag = "ternvale",
            "attached virtio-vsock and virtio-9p"
        );
        Ok(vec![(vs_base, vs_size, vsock), (p9_base, p9_size, p9)])
    });
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let control_result = controller
        .join()
        .unwrap_or_else(|_| Err("controller panicked".to_string()));
    cleanup(&uds_dir, &share_host);

    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(&log_dir, &exit, &script_result, &serial);
    let fail = |why: String| {
        format!(
            "{why}\nexit={exit:?}\nlog={}\n--- serial ---\n{}",
            guard.log_path().display(),
            last_lines(&serial, 60)
        )
    };
    script_result.map_err(fail)?;
    control_result.map_err(fail)?;
    if !matches!(exit, Ok(ExitReason::SystemOff)) {
        return Err(fail("guest did not power off".into()));
    }
    tracing::info!(
        target: "ternvale::boot",
        total_ms = started.elapsed().as_millis() as u64,
        scenario = "share-clipboard",
        "boot harness passed"
    );
    drop(guard);
    Ok(())
}

fn control(
    server: AgentServer,
    _sock: &Path,
    serial_log: &Path,
    share_host: &Path,
    expected_host_sha: &str,
) -> Result<(), String> {
    server
        .wait_for(Duration::from_secs(60), |s| {
            s.state == AgentState::Connected && s.pongs >= 1
        })
        .ok_or_else(|| format!("agent never connected: {:?}", server.status()))?;
    tracing::info!(target: "ternvale::boot", "agent connected; waiting for guest share mount");
    wait_serial(serial_log, "SHARE_MOUNTED", Duration::from_secs(60))?;

    tracing::info!(target: "ternvale::boot", len = HOST_CLIP.len(), "host clipboard_set to guest");
    server
        .send(&Message::ClipboardSet {
            text: ClipboardText(HOST_CLIP.into()),
        })
        .map_err(|e| e.to_string())?;

    wait_serial(serial_log, "CLIP_HOST_OK", Duration::from_secs(30))?;
    tracing::info!(target: "ternvale::boot", "guest confirmed host clipboard in terminal harness");

    let guest_text = server
        .clipboard_get(Duration::from_secs(10))
        .map_err(|e| e.to_string())?;
    if guest_text != GUEST_CLIP {
        return Err(format!(
            "clipboard_get expected {:?}, got {:?}",
            GUEST_CLIP, guest_text
        ));
    }
    tracing::info!(
        target: "ternvale::boot",
        len = guest_text.len(),
        "host clipboard_get from guest ok"
    );

    let serial = std::fs::read_to_string(serial_log).map_err(|e| e.to_string())?;
    let host_line = serial
        .lines()
        .find(|line| line.starts_with("HOST_FILE_SHA="))
        .ok_or("serial missing HOST_FILE_SHA")?;
    let guest_sha = host_line
        .trim_start_matches("HOST_FILE_SHA=")
        .trim()
        .to_string();
    if guest_sha != expected_host_sha {
        return Err(format!(
            "guest sha256 of from_host.bin {guest_sha} != host {expected_host_sha}"
        ));
    }
    tracing::info!(
        target: "ternvale::boot",
        sha256 = %guest_sha,
        bytes = LARGE_BYTES,
        "guest verified 100 MiB host file checksum"
    );

    let guest_line = serial
        .lines()
        .find(|line| line.starts_with("GUEST_FILE_SHA="))
        .ok_or("serial missing GUEST_FILE_SHA")?;
    let guest_file_sha = guest_line.trim_start_matches("GUEST_FILE_SHA=").trim();
    let host_guest_path = share_host.join("from_guest.bin");
    let on_host_sha = sha256_file(&host_guest_path)?;
    if on_host_sha != guest_file_sha {
        return Err(format!(
            "host sha256 of from_guest.bin {on_host_sha} != guest reported {guest_file_sha}"
        ));
    }
    tracing::info!(
        target: "ternvale::boot",
        path = %host_guest_path.display(),
        sha256 = %on_host_sha,
        "host sees guest-created share file immediately"
    );

    wait_serial(
        serial_log,
        "SHARE_CLIPBOARD_TEST_DONE",
        Duration::from_secs(30),
    )?;
    Ok(())
}

fn wait_serial(path: &Path, needle: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let text = std::fs::read_to_string(path).unwrap_or_default();
        if text.contains(needle) {
            tracing::debug!(target: "ternvale::boot", needle, "serial marker seen");
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(format!("timed out waiting for serial marker {needle:?}"))
}

fn write_large_file(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut written = 0u64;
    let mut seed = 0u8;
    while written < LARGE_BYTES {
        let take = CHUNK.min((LARGE_BYTES - written) as usize);
        let mut chunk = vec![seed; take];
        for (i, byte) in chunk.iter_mut().enumerate() {
            *byte = seed.wrapping_add((i % 251) as u8);
        }
        file.write_all(&chunk).map_err(|e| e.to_string())?;
        seed = seed.wrapping_add(1);
        written += take as u64;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    sha256_file(path)
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let path_arg = path.display().to_string();
    let out = std::process::Command::new("shasum")
        .args(["-a", "256", &path_arg])
        .output()
        .map_err(|e| format!("shasum: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "shasum failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let line = String::from_utf8_lossy(&out.stdout);
    line.split_whitespace()
        .next()
        .map(|s| s.to_string())
        .ok_or_else(|| "shasum produced no digest".to_string())
}

fn cleanup(uds: &Path, share: &Path) {
    if let Err(error) = std::fs::remove_dir_all(uds) {
        tracing::warn!(target: "ternvale::boot", dir = %uds.display(), error = %error, "uds cleanup failed");
    }
    if let Err(error) = std::fs::remove_dir_all(share) {
        tracing::warn!(target: "ternvale::boot", dir = %share.display(), error = %error, "share cleanup failed");
    }
}

fn script() -> Vec<Step> {
    let banner = banner_timeout();
    vec![
        Step::Expect {
            pattern: "Linux version".to_string(),
            timeout: banner,
        },
        Step::Expect {
            pattern: "ternvale devices ready".to_string(),
            timeout: banner,
        },
        Step::Expect {
            pattern: "ternvale agent started".to_string(),
            timeout: banner,
        },
        Step::Expect {
            pattern: "SHARE_CLIPBOARD_TEST_DONE".to_string(),
            timeout: Duration::from_secs(180),
        },
    ]
}
