//! `ternvale run <config>`: boot a VM and serve its control socket until it stops.
//!
//! Guest serial goes to stdout and `serial_log`; host logs go to stderr and
//! `~/Library/Logs/Ternvale`. Config disks are virtio-blk on virtio-mmio
//! slots `0..d`, NICs are virtio-net on the slots after them, and `[vsock]`
//! adds virtio-vsock on the next slot plus the guest agent server.

use std::path::Path;
use std::process::ExitCode;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use ternvale_config::VmConfig;
use ternvale_devices::{
    attach_disks, open_backend, Pl011, VirtioNet, VirtioVsock, VsockConfig, DEFAULT_MAC,
};
use ternvale_log::LogConfig;
use ternvale_vmm::{DeviceAttach, Machine, MachineError, MmioDevice, VmControl, VmState};

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
    let config = VmConfig::from_path(config_path)
        .with_context(|| format!("load vm config {}", config_path.display()))?;
    let log_dir = LogConfig::default_log_dir().context("find the log directory")?;
    let guard = ternvale_log::init(LogConfig::new(config.name.clone(), log_dir))
        .with_context(|| format!("initialize logging for vm {}", config.name))?;
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

    let socket = paths::socket_path(&config.name)?;
    let control = Arc::new(VmControl::new(&config.name, config.cpus));
    let vsock = crate::vsock::prepare(&config, &socket)
        .with_context(|| format!("set up vsock for vm {}", config.name))?;
    let agent = vsock.as_ref().and_then(|setup| setup.agent.clone());
    let server = ControlServer::with_agent(&socket, Arc::clone(&control), agent)
        .with_context(|| format!("start the control socket for vm {}", config.name))?;
    let reaper = spawn_reaper(Arc::clone(&control), socket.clone())?;
    let uart = Pl011::open(&config.serial_log)
        .with_context(|| format!("open serial log {}", config.serial_log.display()))?;

    let disks: Vec<(std::path::PathBuf, bool)> = config
        .disks
        .iter()
        .map(|disk| (disk.path.clone(), disk.read_only))
        .collect();
    let nics: Vec<String> = config.nics.iter().map(|nic| nic.backend.clone()).collect();
    let device = vsock.as_ref().map(|setup| setup.device.clone());
    let exit = Machine::run_controlled(&config, Box::new(uart), &control, move |attach| {
        attach_devices(attach, &disks, &nics, device.as_ref())
    });

    if reaper.join().is_err() {
        tracing::error!(target: "ternvale::cli", "force-stop reaper thread panicked");
    }
    drop(server);
    drop(vsock);
    let status = control.status();
    let code = match &exit {
        Ok(reason) => {
            tracing::info!(target: "ternvale::cli", ?reason, state = %status.state, "vm stopped");
            eprintln!("ternvale: vm {} stopped ({reason:?})", config.name);
            ExitCode::SUCCESS
        }
        Err(error) => {
            tracing::error!(target: "ternvale::cli", error = %error, state = %status.state, "vm failed");
            eprintln!("ternvale: vm {} failed: {error}", config.name);
            ExitCode::FAILURE
        }
    };
    drop(_entered);
    drop(guard);
    Ok(code)
}

type Attached = Vec<(u64, u64, Box<dyn MmioDevice>)>;

/// Disks on slots `0..d`, then NICs, then vsock. MACs count up from [`DEFAULT_MAC`].
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(disks = disks.len(), nics = nics.len(), vsock = vsock.is_some()))]
fn attach_devices(
    attach: &DeviceAttach,
    disks: &[(std::path::PathBuf, bool)],
    nics: &[String],
    vsock: Option<&VsockConfig>,
) -> Result<Attached, MachineError> {
    let mut devices = attach_disks(attach, disks)?;
    for (index, backend_name) in nics.iter().enumerate() {
        let slot = u32::try_from(disks.len() + index)
            .map_err(|_| MachineError::Attach(format!("nic {index} slot does not fit in u32")))?;
        let fail = |error: ternvale_devices::NetError| {
            MachineError::Attach(format!("nic {index} (backend {backend_name}): {error}"))
        };
        let mut mac = DEFAULT_MAC;
        mac[5] = mac[5].wrapping_add(index as u8);
        let backend = open_backend(backend_name).map_err(fail)?;
        let (base, size, device, _stats) = VirtioNet::attach(
            slot,
            mac,
            backend,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(slot),
        )
        .map_err(fail)?;
        tracing::info!(target: "ternvale::cli", slot, backend = %backend_name, mac = ?mac, "attached virtio-net");
        devices.push((base, size, device));
    }
    if let Some(vsock) = vsock {
        let slot = u32::try_from(disks.len() + nics.len())
            .map_err(|_| MachineError::Attach("vsock slot does not fit in u32".into()))?;
        let (base, size, device, _stats, cid) = VirtioVsock::attach(
            slot,
            vsock,
            Arc::clone(&attach.memory),
            attach.virtio_irq_hook(slot),
        )
        .map_err(|error| MachineError::Attach(format!("vsock: {error}")))?;
        tracing::info!(target: "ternvale::cli", slot, cid, uds_dir = %vsock.uds_dir.display(), "attached virtio-vsock");
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
