//! `ternvale run <config>`: boot a VM and serve its control socket until it stops.
//!
//! Guest serial goes to stdout and `serial_log`; host logs go to stderr and
//! `~/Library/Logs/Ternvale`. Config disks are virtio-blk on virtio-mmio
//! slots `0..d`, NICs are virtio-net on the slots after them, and `[vsock]`
//! adds virtio-vsock on the next slot plus the guest agent server.

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use ternvale_config::VmConfig;
use ternvale_devices::{
    open_backend, Pl011, VirtioBlk, VirtioNet, VirtioVsock, VsockConfig, DEFAULT_MAC,
};
use ternvale_log::LogConfig;
use ternvale_vmm::{
    lockwatch, DeviceAttach, Machine, MachineError, MmioDevice, VmControl, VmState,
};

use crate::diag::{DiagSink, RunManifest, SharedDevices};
use crate::paths;
use crate::server::ControlServer;

/// After `force-stop`, how long teardown may take before the process exits.
pub const FORCE_GRACE: Duration = Duration::from_secs(3);
/// Exit code when the force-stop grace runs out.
pub const FORCED_EXIT: i32 = 2;

const REAPER_POLL: Duration = Duration::from_millis(100);

/// Boot `config_path` and block until the VM stops.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(config = %config_path.display()))]
pub fn run(config_path: &Path) -> Result<ExitCode> {
    let load = || format!("load vm config {}", config_path.display());
    let text = std::fs::read_to_string(config_path).with_context(load)?;
    let config = VmConfig::parse(&text).with_context(load)?;
    let log_dir = LogConfig::default_log_dir().context("find the log directory")?;
    // Logging starts before validation so a rejected config (a missing kernel,
    // say) lands in the VM's host log for `ternvale logs` and `ternvale report`.
    let guard = match ternvale_log::init(LogConfig::new(config.name.clone(), log_dir)) {
        Ok(guard) => guard,
        Err(error) => {
            config.validate().with_context(load)?;
            return Err(error)
                .with_context(|| format!("initialize logging for vm {}", config.name));
        }
    };
    let span = tracing::info_span!(target: "ternvale::cli", "vm", vm = %config.name);
    let _entered = span.enter();
    tracing::info!(
        target: "ternvale::cli",
        config = %config_path.display(),
        host_log = %guard.log_path().display(),
        serial_log = %config.serial_log.display(),
        cpus = config.cpus,
        ram_mib = config.ram_mib,
        "ternvale run"
    );

    write_manifest(&config, config_path, guard.log_path());
    if let Err(error) = config.validate() {
        tracing::error!(target: "ternvale::cli", config = %config_path.display(), error = %error, "vm config rejected; not booting");
        return Err(error).with_context(load);
    }
    warn_config_problems(&config);

    let socket = paths::socket_path(&config.name)?;
    let control = Arc::new(VmControl::new(&config.name, config.cpus));
    let vsock = crate::vsock::prepare(&config, &socket)
        .with_context(|| format!("set up vsock for vm {}", config.name))?;
    let agent = vsock.as_ref().and_then(|setup| setup.agent.clone());
    let stats = SharedDevices::default();
    let diag = Arc::new(DiagSink::new(
        guard.log_path(),
        Arc::clone(&control),
        agent.clone(),
        Arc::clone(&stats),
    ));
    let server = ControlServer::with_diag(&socket, Arc::clone(&control), agent, Arc::clone(&diag))
        .with_context(|| format!("start the control socket for vm {}", config.name))?;
    let reaper = spawn_reaper(Arc::clone(&control), socket.clone())?;
    let uart = Pl011::open(&config.serial_log)
        .with_context(|| format!("open serial log {}", config.serial_log.display()))?;

    let disks: Vec<(PathBuf, bool)> = config
        .disks
        .iter()
        .map(|disk| (disk.path.clone(), disk.read_only))
        .collect();
    let nics: Vec<String> = config.nics.iter().map(|nic| nic.backend.clone()).collect();
    let device = vsock.as_ref().map(|setup| setup.device.clone());
    let exit = Machine::run_controlled(&config, Box::new(uart), &control, move |attach| {
        attach_devices(attach, &disks, &nics, device.as_ref(), &stats)
    });

    if reaper.join().is_err() {
        tracing::error!(target: "ternvale::cli", "force-stop reaper thread panicked");
    }
    drop(server);
    let status = control.status();
    let code = match &exit {
        Ok(reason) => {
            tracing::info!(target: "ternvale::cli", ?reason, state = %status.state, "vm stopped");
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(target: "ternvale::cli", error = %error, state = %status.state, "vm failed");
            ExitCode::FAILURE
        }
    };
    let exit = exit.map_err(|error| error.to_string());
    let summary = diag.summary(Some(exit.clone()));
    crate::summary::log(&summary);
    eprintln!("ternvale: {}", crate::summary::render(&summary));
    if let Err(error) = diag.dump(Some(exit)) {
        tracing::warn!(target: "ternvale::cli", error = %format!("{error:#}"), "could not write stop-time diagnostics");
    }
    drop(vsock);
    drop(_entered);
    drop(guard);
    Ok(code)
}

/// Record the `.run.json` a crash report reads; a failure only costs the report its config.
fn write_manifest(config: &VmConfig, config_path: &Path, host_log: &Path) {
    let manifest = RunManifest {
        name: config.name.clone(),
        pid: std::process::id(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        config: std::fs::canonicalize(config_path).unwrap_or_else(|_| config_path.to_path_buf()),
        serial_log: config.serial_log.clone(),
        host_log: host_log.to_path_buf(),
        started: chrono::Local::now().to_rfc3339(),
    };
    if let Err(error) = crate::diag::write_manifest(&manifest) {
        tracing::warn!(target: "ternvale::cli", error = %format!("{error:#}"), "could not write run manifest");
    }
}

/// Log what `doctor --config` would flag. The VM still boots: the checks are
/// heuristics, and the guest's own output may say more.
fn warn_config_problems(config: &VmConfig) {
    use crate::doctor::Outcome;
    for check in crate::doctor::vm::checks(config) {
        if matches!(check.outcome, Outcome::Warn | Outcome::Fail) {
            tracing::warn!(
                target: "ternvale::cli",
                check = %check.name,
                outcome = ?check.outcome,
                detail = %check.detail,
                fix = check.fix.as_deref().unwrap_or(""),
                "vm config problem"
            );
        }
    }
}

type Attached = Vec<(u64, u64, Box<dyn MmioDevice>)>;

/// Disks on slots `0..d`, then NICs, then vsock. MACs count up from
/// [`DEFAULT_MAC`]. Each device's counters go into `stats`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(disks = disks.len(), nics = nics.len(), vsock = vsock.is_some()))]
fn attach_devices(
    attach: &DeviceAttach,
    disks: &[(PathBuf, bool)],
    nics: &[String],
    vsock: Option<&VsockConfig>,
    stats: &SharedDevices,
) -> Result<Attached, MachineError> {
    let mut stats = lockwatch::lock(stats, "device-stats");
    let mut devices = Vec::new();
    for (index, (path, read_only)) in disks.iter().enumerate() {
        let slot = u32::try_from(index)
            .map_err(|_| MachineError::Attach(format!("disk {index} slot does not fit in u32")))?;
        let (base, size, device, disk_stats) = VirtioBlk::attach(
            slot,
            path,
            *read_only,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(slot),
        )
        .map_err(|error| MachineError::Attach(format!("disk {index}: {error}")))?;
        tracing::info!(target: "ternvale::cli", slot, path = %path.display(), read_only = *read_only, "attached virtio-blk");
        stats.disks.push((path.clone(), disk_stats));
        devices.push((base, size, device));
    }
    for (index, backend_name) in nics.iter().enumerate() {
        let slot = u32::try_from(disks.len() + index)
            .map_err(|_| MachineError::Attach(format!("nic {index} slot does not fit in u32")))?;
        let fail = |error: ternvale_devices::NetError| {
            MachineError::Attach(format!("nic {index} (backend {backend_name}): {error}"))
        };
        let mut mac = DEFAULT_MAC;
        mac[5] = mac[5].wrapping_add(index as u8);
        let backend = open_backend(backend_name).map_err(fail)?;
        let (base, size, device, nic_stats) = VirtioNet::attach(
            slot,
            mac,
            backend,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(slot),
        )
        .map_err(fail)?;
        tracing::info!(target: "ternvale::cli", slot, backend = %backend_name, mac = ?mac, "attached virtio-net");
        stats.nics.push((backend_name.clone(), nic_stats));
        devices.push((base, size, device));
    }
    if let Some(vsock) = vsock {
        let slot = u32::try_from(disks.len() + nics.len())
            .map_err(|_| MachineError::Attach("vsock slot does not fit in u32".into()))?;
        let (base, size, device, vsock_stats, cid) = VirtioVsock::attach(
            slot,
            vsock,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(slot),
        )
        .map_err(|error| MachineError::Attach(format!("vsock: {error}")))?;
        tracing::info!(target: "ternvale::cli", slot, cid, uds_dir = %vsock.uds_dir.display(), "attached virtio-vsock");
        stats.vsock = Some(vsock_stats);
        devices.push((base, size, device));
    }
    Ok(devices)
}

/// Exit the process if a `force-stop` is not done within [`FORCE_GRACE`].
fn spawn_reaper(
    control: Arc<VmControl>,
    socket: std::path::PathBuf,
) -> Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("force-stop".to_string())
        .spawn(move || loop {
            let state = control.wait_terminal(REAPER_POLL);
            if state.is_terminal() {
                tracing::debug!(target: "ternvale::cli", %state, "force-stop reaper done");
                return;
            }
            if control
                .forced_for()
                .is_some_and(|waited| waited >= FORCE_GRACE)
            {
                reap(&control, state, &socket);
            }
        })
        .context("spawn the force-stop reaper thread")
}

fn reap(control: &VmControl, state: VmState, socket: &Path) -> ! {
    if let Err(error) = std::fs::remove_file(socket) {
        tracing::warn!(target: "ternvale::cli", path = %socket.display(), error = %error, "could not remove control socket");
    }
    tracing::error!(
        target: "ternvale::cli",
        vm = control.name(),
        %state,
        grace_ms = FORCE_GRACE.as_millis() as u64,
        code = FORCED_EXIT,
        "force-stop grace expired; exiting without teardown"
    );
    eprintln!(
        "ternvale: vm {} did not stop within {:?} of force-stop; exiting",
        control.name(),
        FORCE_GRACE
    );
    // The log guard lives on the main thread; give the non-blocking file writer
    // a moment to drain before the process ends under it.
    std::thread::sleep(Duration::from_millis(200));
    std::process::exit(FORCED_EXIT);
}
