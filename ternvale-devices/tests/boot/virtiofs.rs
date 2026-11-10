//! Live virtio-fs: POSIX subset, symlink/`..` denials, 1 GiB copy, read-only EROFS.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use ternvale_agent_proto::AGENT_PORT;
use ternvale_config::VmConfig;
use ternvale_devices::{
    host_socket_path, last_lines, AgentServer, AgentServerConfig, AgentState, Pl011, Step,
    VirtioVsock, VsockConfig,
};
use ternvale_vmm::{ExitReason, Machine, MachineError};

use crate::common::{
    assets_root, banner_timeout, boot_cpus, drive, init_logging_at, log_dir, restore_stdin,
    stdin_pipe, write_result,
};

const GUEST_CID: u32 = 3;
const GIB: u64 = 1024 * 1024 * 1024;
const CHUNK: usize = 1024 * 1024;
const LOG_FILTER: &str = "info,ternvale::virtiofs=debug,ternvale::agent=debug,ternvale::boot=debug";

#[tracing::instrument(level = "info", target = "ternvale::boot", skip_all)]
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
    let guard = init_logging_at("boot-virtiofs", &log_dir, LOG_FILTER)?;
    rw_pass(&root, &log_dir, &guard)?;
    ro_pass(&root, &log_dir, &guard)?;
    tracing::info!(target: "ternvale::boot", scenario = "virtiofs", "boot harness passed");
    Ok(())
}

fn rw_pass(assets: &Path, log_dir: &Path, guard: &ternvale_log::LogGuard) -> Result<(), String> {
    let share = std::env::temp_dir().join(format!("tvfs-{}", std::process::id()));
    std::fs::create_dir_all(&share).map_err(|e| format!("share dir: {e}"))?;
    let large = share.join("from_host.bin");
    let host_sha = write_gib(&large)?;
    tracing::info!(
        target: "ternvale::boot",
        path = %large.display(),
        bytes = GIB,
        sha256 = %host_sha,
        "prepared 1 GiB host share file"
    );
    let done = "SHARE_FSTEST_DONE";
    let (serial, host_log, exit) = boot_vm(
        assets,
        "boot-virtiofs",
        "ternvale.agent=debug ternvale.fstest=1",
        share.clone(),
        false,
        done,
        &log_dir,
        &guard,
    )?;
    let fail = |why: String| {
        format!(
            "{why}\nexit={exit:?}\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 80)
        )
    };
    if !serial.contains("SHARE_MOUNTED") || !serial.contains("type=virtiofs") {
        return Err(fail(
            "guest did not mount virtiofs (need CONFIG_VIRTIO_FS / virtiofs.ko)".into(),
        ));
    }
    if !serial.contains("POSIX_SUBSET_OK") {
        return Err(fail("POSIX subset failed".into()));
    }
    check_security(&serial, &host_log).map_err(fail)?;
    let guest_h2g =
        serial_field(&serial, "HOST_FILE_SHA=").ok_or_else(|| fail("no HOST_FILE_SHA".into()))?;
    if guest_h2g != host_sha {
        return Err(fail(format!("h2g sha {guest_h2g} != {host_sha}")));
    }
    let guest_g2h =
        serial_field(&serial, "GUEST_FILE_SHA=").ok_or_else(|| fail("no GUEST_FILE_SHA".into()))?;
    let on_host = sha256_file(&share.join("from_guest.bin")).map_err(fail)?;
    if on_host != guest_g2h {
        return Err(fail(format!("g2h sha {on_host} != {guest_g2h}")));
    }
    tracing::info!(
        target: "ternvale::boot",
        h2g = %serial_field(&serial, "COPY_H2G_UPTIME_S=").unwrap_or_default(),
        g2h = %serial_field(&serial, "COPY_G2H_UPTIME_S=").unwrap_or_default(),
        sha256 = %host_sha,
        bytes = GIB,
        "1 GiB copy both directions matched"
    );
    if !matches!(exit, Ok(ExitReason::SystemOff)) {
        return Err(fail("rw guest did not power off".into()));
    }
    let _ = std::fs::remove_dir_all(&share);
    Ok(())
}

fn ro_pass(assets: &Path, log_dir: &Path, guard: &ternvale_log::LogGuard) -> Result<(), String> {
    let share = std::env::temp_dir().join(format!("tvfs-ro-{}", std::process::id()));
    std::fs::create_dir_all(&share).map_err(|e| format!("ro share: {e}"))?;
    let done = "SHARE_ROTEST_DONE";
    let (serial, host_log, exit) = boot_vm(
        assets,
        "boot-virtiofs-ro",
        "ternvale.agent=debug ternvale.rotest=1",
        share.clone(),
        true,
        done,
        &log_dir,
        &guard,
    )?;
    let fail = |why: String| {
        format!(
            "{why}\nexit={exit:?}\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 40)
        )
    };
    if !serial.contains("WRITE_RC=") || serial.contains("WRITE_RC=0") {
        return Err(fail("read-only share accepted a write".into()));
    }
    let logs = std::fs::read_to_string(&host_log).unwrap_or_default();
    if !logs.contains("share is read-only") && !serial.to_lowercase().contains("read-only") {
        return Err(fail("missing EROFS / read-only reason in logs".into()));
    }
    tracing::info!(target: "ternvale::boot", "read-only share rejected writes");
    if !matches!(exit, Ok(ExitReason::SystemOff)) {
        return Err(fail("ro guest did not power off".into()));
    }
    let _ = std::fs::remove_dir_all(&share);
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn boot_vm(
    assets: &Path,
    name: &str,
    cmdline: &str,
    export: PathBuf,
    read_only: bool,
    done: &'static str,
    log_dir: &Path,
    guard: &ternvale_log::LogGuard,
) -> Result<(String, PathBuf, Result<ExitReason, MachineError>), String> {
    let uds = std::env::temp_dir().join(format!("{name}-uds-{}", std::process::id()));
    std::fs::create_dir_all(&uds).map_err(|e| e.to_string())?;
    let agent_sock = host_socket_path(&uds, AGENT_PORT);
    let shares = vec![("ternvale".to_string(), "/mnt/ternvale".to_string())];
    let server = AgentServer::start_with(&agent_sock, name, AgentServerConfig::default(), shares)
        .map_err(|e| e.to_string())?;
    let serial_log = log_dir.join(format!("{name}-serial.log"));
    let vm = VmConfig {
        name: name.to_string(),
        profile: Default::default(),
        iso: None,
        cpus: boot_cpus(1)?,
        ram_mib: 1024,
        ram_min_mib: None,
        kernel: assets.join("Image"),
        initrd: Some(assets.join("initramfs.cpio")),
        cmdline: cmdline.to_string(),
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
        power: Default::default(),
        encryption: false,
        agent: Default::default(),
        backend: Default::default(),
    };
    let uart = Pl011::open(&serial_log).map_err(|e| e.to_string())?;
    let cancel = Arc::new(AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host_log = guard.log_path().to_path_buf();
    let host_for_feed = host_log.clone();
    let banner = banner_timeout();
    let steps = vec![
        Step::Expect {
            pattern: "Linux version".into(),
            timeout: banner,
        },
        Step::Expect {
            pattern: "ternvale devices ready".into(),
            timeout: banner,
        },
        Step::Expect {
            pattern: done.into(),
            timeout: Duration::from_secs(600),
        },
    ];
    let feeder = std::thread::spawn(move || {
        drive(&serial_for_feed, &mut input, &flag, &host_for_feed, steps)
    });
    let serial_wait = serial_log.clone();
    let controller = std::thread::spawn(move || {
        server
            .wait_for(Duration::from_secs(90), |s| {
                s.state == AgentState::Connected && s.pongs >= 1
            })
            .ok_or_else(|| format!("agent never connected: {:?}", server.status()))?;
        wait_serial(&serial_wait, done, Duration::from_secs(600))
    });
    let vsock_config = VsockConfig {
        guest_cid: Some(GUEST_CID),
        uds_dir: uds.clone(),
        listen_ports: Vec::new(),
    };
    let opts = ternvale_virtiofs::Options {
        read_only,
        ..ternvale_virtiofs::Options::default()
    };
    let started = Instant::now();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        let (vs_base, vs_size, vsock, _s, cid) = VirtioVsock::attach(
            0,
            &vsock_config,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(0),
        )
        .map_err(|e| MachineError::Attach(e.to_string()))?;
        let (fs_base, fs_size, fs, _st) = ternvale_virtiofs::VirtioFs::attach(
            1,
            export,
            "ternvale",
            opts,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(1),
        )
        .map_err(|e| MachineError::Attach(e.to_string()))?;
        tracing::info!(
            target: "ternvale::boot",
            guest_cid = cid,
            read_only,
            "attached virtio-vsock and virtio-fs"
        );
        Ok(vec![(vs_base, vs_size, vsock), (fs_base, fs_size, fs)])
    });
    cancel.store(true, Ordering::Release);
    let _ = feeder.join();
    restore_stdin(saved);
    let _ = controller.join();
    tracing::info!(
        target: "ternvale::boot",
        name,
        total_ms = started.elapsed().as_millis() as u64,
        "virtiofs VM finished"
    );
    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(log_dir, &exit, &Ok(()), &serial);
    let _ = std::fs::remove_dir_all(&uds);
    Ok((serial, host_log, exit))
}

fn check_security(serial: &str, host_log: &Path) -> Result<(), String> {
    let open_dd = serial_int(serial, "OPEN_DOTDOT rc=")?;
    let rename_dd = serial_int(serial, "RENAME_DOTDOT rc=")?;
    // Linux openat(mount_fd, "..") stays on the mount (fd to the share root)
    // and does not send FUSE LOOKUP `..` to the host. Escape is rename of a
    // file onto `..` or following `../outside_secret`.
    tracing::debug!(
        target: "ternvale::virtiofs",
        open_dotdot = open_dd,
        rename_dotdot = rename_dd,
        "virtiofs guest .. probes"
    );
    if rename_dd >= 0 {
        return Err(format!(
            "`..` rename escaped the share rename={rename_dd} open={open_dd}"
        ));
    }
    let out_open = serial_int(serial, "OPEN_OUT_LINK rc=")?;
    if out_open >= 0 {
        return Err("read through ../outside symlink succeeded".into());
    }
    let logs = std::fs::read_to_string(host_log).unwrap_or_default();
    if !logs.contains("rejected share") {
        return Err("host log has no WARN 'rejected share' for the denials".into());
    }
    if let Some(n) = serial_field(serial, "PASSWD_READ n=") {
        if n.parse::<i32>().unwrap_or(0) > 0 {
            let host_passwd = std::fs::read("/etc/passwd").unwrap_or_default();
            let snippet =
                String::from_utf8_lossy(&host_passwd[..host_passwd.len().min(20)]).into_owned();
            if !snippet.is_empty() && serial.contains(&snippet) {
                return Err("guest read host /etc/passwd through the share".into());
            }
        }
    }
    tracing::info!(target: "ternvale::boot", "symlink and dot-dot probes denied");
    Ok(())
}

fn serial_field<'a>(serial: &'a str, prefix: &str) -> Option<&'a str> {
    serial
        .lines()
        .find_map(|line| line.strip_prefix(prefix).map(str::trim))
}

fn serial_int(serial: &str, prefix: &str) -> Result<i32, String> {
    serial
        .lines()
        .find_map(|line| {
            line.strip_prefix(prefix)
                .and_then(|rest| rest.split_whitespace().next())
                .and_then(|tok| tok.parse().ok())
        })
        .ok_or_else(|| format!("serial missing {prefix}"))
}

fn wait_serial(path: &Path, needle: &str, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if std::fs::read_to_string(path)
            .unwrap_or_default()
            .contains(needle)
        {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(format!("timed out waiting for {needle:?}"))
}

fn write_gib(path: &Path) -> Result<String, String> {
    let mut file = std::fs::File::create(path).map_err(|e| e.to_string())?;
    let mut written = 0u64;
    let mut seed = 1u8;
    let mut chunk = vec![0u8; CHUNK];
    while written < GIB {
        let take = CHUNK.min((GIB - written) as usize);
        for (i, byte) in chunk[..take].iter_mut().enumerate() {
            *byte = seed.wrapping_add((i % 251) as u8);
        }
        file.write_all(&chunk[..take]).map_err(|e| e.to_string())?;
        seed = seed.wrapping_add(1);
        written += take as u64;
    }
    file.sync_all().map_err(|e| e.to_string())?;
    sha256_file(path)
}

fn sha256_file(path: &Path) -> Result<String, String> {
    let out = std::process::Command::new("shasum")
        .args(["-a", "256", &path.display().to_string()])
        .output()
        .map_err(|e| format!("shasum: {e}"))?;
    if !out.status.success() {
        return Err(format!(
            "shasum failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .next()
        .map(str::to_string)
        .ok_or_else(|| "shasum produced no digest".into())
}
