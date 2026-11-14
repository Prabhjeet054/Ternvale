//! [`VzBackend`]: Linux guests on Virtualization.framework.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use ternvale_config::{Profile, VmConfig};
use ternvale_vmm::VmState;
use ternvale_vz::{VzError, VzHost, VzPlan};

use crate::{
    AgentChannel, BackendCapabilities, BackendError, BackendStats, DisplaySurface, InputSink,
    MediaSlot, StopMode, VmBackend,
};

struct Shared {
    name: String,
    state: Mutex<VmState>,
    listeners: Mutex<Vec<Sender<VmState>>>,
}

impl Shared {
    fn set_state(&self, to: VmState) {
        *crate::lock_state(&self.state) = to;
        crate::lock_listeners(&self.listeners).retain(|tx| tx.send(to).is_ok());
    }

    fn state(&self) -> VmState {
        *crate::lock_state(&self.state)
    }
}

/// Linux VZ backend. Unit tests and conformance use the stub lifecycle;
/// `ternvale run` opens a live host (`live = true`).
pub struct VzBackend {
    config: VmConfig,
    live: bool,
    host: Mutex<Option<VzHost>>,
    shared: Arc<Shared>,
    media: Mutex<HashMap<MediaSlot, std::path::PathBuf>>,
    uds_dir: Mutex<Option<std::path::PathBuf>>,
    show_view: Mutex<bool>,
    restoring: Arc<AtomicBool>,
}

impl VzBackend {
    /// Stub host: no Virtualization.framework calls (conformance / `open` tests).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::backend",
        skip_all,
        fields(vm = %config.name)
    )]
    pub async fn create(config: VmConfig) -> Result<Self, BackendError> {
        Self::build(config, false)
    }

    /// Live host: Swift `VZVirtualMachine` on start.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::backend",
        skip_all,
        fields(vm = %config.name)
    )]
    pub async fn create_live(config: VmConfig) -> Result<Self, BackendError> {
        Self::build(config, true)
    }

    fn build(config: VmConfig, live: bool) -> Result<Self, BackendError> {
        let _plan = VzPlan::from_config(&config).map_err(BackendError::from)?;
        tracing::info!(
            target: "ternvale::backend",
            vm = %config.name,
            live,
            "vz backend created"
        );
        Ok(Self {
            live,
            host: Mutex::new(None),
            shared: Arc::new(Shared {
                name: config.name.clone(),
                state: Mutex::new(VmState::Created),
                listeners: Mutex::new(Vec::new()),
            }),
            config,
            media: Mutex::new(HashMap::new()),
            uds_dir: Mutex::new(None),
            show_view: Mutex::new(false),
            restoring: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Host Unix socket directory created by `ternvale run` (Step 29 layout).
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(dir = %dir.display()))]
    pub fn set_vsock_uds_dir(&self, dir: &Path) {
        *self.uds_dir.lock().unwrap_or_else(|p| p.into_inner()) = Some(dir.to_path_buf());
    }

    /// `VZVirtualMachineView` in the VMM process (`--display window`).
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::backend",
        skip_all,
        fields(show_view)
    )]
    pub fn set_show_view(&self, show_view: bool) {
        *self.show_view.lock().unwrap_or_else(|p| p.into_inner()) = show_view;
    }

    fn require(&self, allowed: &[VmState], op: &'static str) -> Result<VmState, BackendError> {
        let state = self.shared.state();
        if allowed.contains(&state) {
            Ok(state)
        } else {
            Err(BackendError::InvalidTransition {
                op,
                state: state.as_str().to_string(),
            })
        }
    }

    /// Block until Stopped or Failed (live run loop).
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    pub fn wait_terminal(&self) -> Result<(), BackendError> {
        let show_view = *self.show_view.lock().unwrap_or_else(|p| p.into_inner());
        if self.live && show_view {
            let rx = self.subscribe_state();
            let vm = self.config.name.clone();
            std::thread::Builder::new()
                .name("ternvale.vz.wait".into())
                .spawn(move || {
                    while let Ok(state) = rx.recv() {
                        if state.is_terminal() {
                            tracing::info!(
                                target: "ternvale::backend",
                                vm = %vm,
                                state = %state,
                                "vz terminal (stop NSApplication)"
                            );
                            ternvale_vz::nsapp_stop();
                            break;
                        }
                    }
                })
                .map_err(|error| BackendError::Io {
                    op: "vz_wait_thread",
                    detail: error.to_string(),
                })?;
            tracing::info!(
                target: "ternvale::vz",
                vm = %self.config.name,
                "vz entering NSApplication.run for VirtualMachineView"
            );
            ternvale_vz::nsapp_run();
            return Ok(());
        }
        let rx = self.subscribe_state();
        while let Ok(state) = rx.recv() {
            if state.is_terminal() {
                tracing::info!(
                    target: "ternvale::backend",
                    vm = %self.config.name,
                    state = %state,
                    "vz terminal"
                );
                return Ok(());
            }
        }
        Ok(())
    }

    fn map_host_state(code: i32) -> VmState {
        match code {
            0 => VmState::Created,
            1 => VmState::Running,
            2 => VmState::Paused,
            3 => VmState::Stopping,
            4 => VmState::Stopped,
            _ => VmState::Failed,
        }
    }

    fn host_lock(&self) -> std::sync::MutexGuard<'_, Option<VzHost>> {
        self.host.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn state_hook(&self) -> Arc<dyn Fn(i32, String) + Send + Sync> {
        let shared = Arc::clone(&self.shared);
        let restoring = Arc::clone(&self.restoring);
        Arc::new(move |code, detail: String| {
            let state = VzBackend::map_host_state(code);
            if restoring.load(Ordering::SeqCst) && state.is_terminal() {
                tracing::debug!(
                    target: "ternvale::vz",
                    vm = %shared.name,
                    state = %state,
                    detail = %detail,
                    "ignoring terminal vz state during restore"
                );
                return;
            }
            tracing::info!(
                target: "ternvale::vz",
                vm = %shared.name,
                state = %state,
                detail = %detail,
                "vz delegate/kvo"
            );
            shared.set_state(state);
        })
    }

    fn open_live_host(&self, restore_apple: Option<&Path>) -> Result<(), BackendError> {
        let mut plan = VzPlan::from_config(&self.config).map_err(BackendError::from)?;
        if let Some(dir) = self
            .uds_dir
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .as_ref()
        {
            plan.uds_dir = dir.display().to_string();
        }
        plan.show_view = *self.show_view.lock().unwrap_or_else(|p| p.into_inner());
        tracing::info!(
            target: "ternvale::macos",
            vm = %self.config.name,
            show_view = plan.show_view,
            restore = restore_apple.is_some(),
            "macos vz start devices"
        );
        let host = VzHost::create(&plan, self.state_hook()).map_err(BackendError::from)?;
        if let Some(apple) = restore_apple {
            host.restore(&apple.display().to_string())
                .map_err(BackendError::from)?;
            host.resume().map_err(BackendError::from)?;
        } else {
            host.start().map_err(map_vz_start)?;
        }
        *self.host_lock() = Some(host);
        if matches!(
            self.shared.state(),
            VmState::Created | VmState::Stopping | VmState::Stopped | VmState::Paused
        ) {
            self.shared.set_state(VmState::Running);
        }
        Ok(())
    }
}

fn map_vz_start(error: VzError) -> BackendError {
    let text = error.to_string();
    if text.contains("exceeds the limit")
        || text.contains("maximum supported number")
        || text.contains("VirtualMachineLimitExceeded")
    {
        tracing::warn!(
            target: "ternvale::macos",
            error = %text,
            "vz start hit Apple macos guest limit"
        );
        return BackendError::Io {
            op: "macos_guest_limit",
            detail: format!(
                "cannot start a macOS guest: Apple's Virtualization.framework allows two concurrent macOS VMs (VZErrorVirtualMachineLimitExceeded; SLA §2B(iii)). Stop one first. framework: {text}"
            ),
        };
    }
    BackendError::from(error)
}

impl VmBackend for VzBackend {
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    fn capabilities(&self) -> BackendCapabilities {
        if self.config.profile == ternvale_config::Profile::Macos {
            BackendCapabilities::macos_vz()
        } else {
            BackendCapabilities::linux_vz()
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name, live = self.live))]
    async fn start(&self) -> Result<(), BackendError> {
        match self.require(&[VmState::Created, VmState::Running], "start")? {
            VmState::Running => Ok(()),
            VmState::Created if !self.live => {
                self.shared.set_state(VmState::Running);
                Ok(())
            }
            VmState::Created => {
                if self.config.profile == Profile::Macos {
                    crate::refuse_if_at_limit(&self.config.name)?;
                }
                self.open_live_host(None)
            }
            _ => Err(BackendError::InvalidTransition {
                op: "start",
                state: self.state().as_str().to_string(),
            }),
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    async fn pause(&self) -> Result<(), BackendError> {
        self.require(&[VmState::Running, VmState::Paused], "pause")?;
        if let Some(host) = self.host_lock().as_ref() {
            host.pause().map_err(BackendError::from)?;
        }
        self.shared.set_state(VmState::Paused);
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    async fn resume(&self) -> Result<(), BackendError> {
        self.require(&[VmState::Paused, VmState::Running], "resume")?;
        if let Some(host) = self.host_lock().as_ref() {
            host.resume().map_err(BackendError::from)?;
        }
        self.shared.set_state(VmState::Running);
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name, ?mode))]
    async fn stop(&self, mode: StopMode) -> Result<(), BackendError> {
        self.require(
            &[
                VmState::Created,
                VmState::Running,
                VmState::Paused,
                VmState::Stopping,
            ],
            "stop",
        )?;
        let force = matches!(mode, StopMode::Force);
        if let Some(host) = self.host_lock().as_ref() {
            host.stop(force).map_err(BackendError::from)?;
        }
        self.shared.set_state(VmState::Stopped);
        Ok(())
    }

    fn state(&self) -> VmState {
        self.shared.state()
    }

    fn subscribe_state(&self) -> Receiver<VmState> {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(self.state());
        crate::lock_listeners(&self.shared.listeners).push(tx);
        rx
    }

    async fn query_stats(&self) -> Result<BackendStats, BackendError> {
        Ok(BackendStats {
            name: self.config.name.clone(),
            state: self.state(),
            cpus: self.config.cpus,
            cpu_entries: self.config.cpus as usize,
        })
    }

    async fn attach_media(&self, slot: MediaSlot, path: &Path) -> Result<(), BackendError> {
        crate::lock_media(&self.media).insert(slot, path.to_path_buf());
        Ok(())
    }

    async fn detach_media(&self, slot: MediaSlot) -> Result<(), BackendError> {
        crate::lock_media(&self.media).remove(&slot);
        Ok(())
    }

    #[tracing::instrument(level = "info", target = "ternvale::snapshot", skip_all, fields(vm = %self.config.name, label, path = %path.display()))]
    async fn snapshot_save(&self, label: &str, path: &Path) -> Result<(), BackendError> {
        if !self.live {
            return Err(BackendError::Unsupported {
                feature: "snapshots",
            });
        }
        std::fs::create_dir_all(path).map_err(|error| BackendError::Io {
            op: "vz_snapshot_mkdir",
            detail: error.to_string(),
        })?;
        let apple = path.join("apple.vzvmsave");
        let wrapped = path.join("state.tvzs");
        {
            let host = self.host_lock();
            let host = host.as_ref().ok_or(BackendError::Unsupported {
                feature: "snapshots",
            })?;
            host.pause().map_err(BackendError::from)?;
            host.save(&apple.display().to_string())
                .map_err(BackendError::from)?;
            // Leave paused: resuming would dirty the disk and make restore
            // (`invalid argument`) fail. WWDC23-10007: copy disks with the save.
        }
        self.shared.set_state(VmState::Paused);
        ternvale_vz::wrap_tvzs(&apple, &wrapped).map_err(BackendError::from)?;
        let label_json = label.replace('\\', "\\\\").replace('"', "\\\"");
        let meta = format!("{{\"format\":\"tvzs\",\"version\":1,\"label\":\"{label_json}\"}}\n");
        std::fs::write(path.join("meta.json"), meta).map_err(|error| BackendError::Io {
            op: "vz_snapshot_meta",
            detail: error.to_string(),
        })?;
        tracing::info!(
            target: "ternvale::snapshot",
            vm = %self.config.name,
            label,
            path = %wrapped.display(),
            "vz machine state saved"
        );
        Ok(())
    }

    #[tracing::instrument(level = "info", target = "ternvale::snapshot", skip_all, fields(vm = %self.config.name, label, path = %path.display()))]
    async fn snapshot_restore(&self, _label: &str, path: &Path) -> Result<(), BackendError> {
        if !self.live {
            return Err(BackendError::Unsupported {
                feature: "snapshots",
            });
        }
        let wrapped = path.join("state.tvzs");
        let apple = path.join("apple.restore.vzvmsave");
        ternvale_vz::unwrap_tvzs(&wrapped, &apple).map_err(BackendError::from)?;
        self.restoring.store(true, Ordering::SeqCst);
        {
            let mut slot = self.host_lock();
            if let Some(host) = slot.as_ref() {
                host.stop(true).map_err(BackendError::from)?;
            }
            *slot = None;
        }
        let result = self.open_live_host(Some(&apple));
        self.restoring.store(false, Ordering::SeqCst);
        result?;
        tracing::info!(
            target: "ternvale::snapshot",
            vm = %self.config.name,
            path = %path.display(),
            "vz machine state restored"
        );
        Ok(())
    }

    async fn display_surface(&self) -> Result<DisplaySurface, BackendError> {
        Err(BackendError::Unsupported {
            feature: "custom_display_pipeline",
        })
    }

    fn input_sink(&self) -> Arc<dyn InputSink> {
        Arc::new(NoopInput)
    }

    async fn agent_channel(&self) -> Result<AgentChannel, BackendError> {
        let (a, b) = UnixStream::pair().map_err(|error| BackendError::Io {
            op: "agent_channel",
            detail: error.to_string(),
        })?;
        drop(b);
        Ok(AgentChannel { stream: a })
    }
}

struct NoopInput;

impl InputSink for NoopInput {
    fn put_key(&self, _keycode: u16, _pressed: bool) -> Result<(), BackendError> {
        Ok(())
    }

    fn put_pointer(&self, _x: u16, _y: u16, _buttons: u8) -> Result<(), BackendError> {
        Ok(())
    }
}

impl From<VzError> for BackendError {
    fn from(error: VzError) -> Self {
        match error {
            VzError::UnsupportedOption { option, capability } => {
                BackendError::UnsupportedOption { option, capability }
            }
            VzError::Host { op, detail } | VzError::Io { op, detail } => {
                BackendError::Io { op, detail }
            }
        }
    }
}
