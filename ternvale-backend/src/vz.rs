//! [`VzBackend`]: Linux guests on Virtualization.framework.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use ternvale_config::VmConfig;
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
        })
    }

    /// Host Unix socket directory created by `ternvale run` (Step 29 layout).
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(dir = %dir.display()))]
    pub fn set_vsock_uds_dir(&self, dir: &Path) {
        *self.uds_dir.lock().unwrap_or_else(|p| p.into_inner()) = Some(dir.to_path_buf());
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
}

impl VmBackend for VzBackend {
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::linux_vz()
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
                let mut plan = VzPlan::from_config(&self.config).map_err(BackendError::from)?;
                if let Some(dir) = self
                    .uds_dir
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_ref()
                {
                    plan.uds_dir = dir.display().to_string();
                }
                let shared = Arc::clone(&self.shared);
                let hook = Arc::new(move |code, detail: String| {
                    let state = VzBackend::map_host_state(code);
                    tracing::info!(
                        target: "ternvale::vz",
                        vm = %shared.name,
                        state = %state,
                        detail = %detail,
                        "vz delegate/kvo"
                    );
                    shared.set_state(state);
                });
                let host = VzHost::create(&plan, hook).map_err(BackendError::from)?;
                host.start().map_err(BackendError::from)?;
                *self.host_lock() = Some(host);
                if matches!(self.shared.state(), VmState::Created | VmState::Stopping) {
                    self.shared.set_state(VmState::Running);
                }
                Ok(())
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

    async fn snapshot_save(&self, _label: &str, _path: &Path) -> Result<(), BackendError> {
        Err(BackendError::Unsupported {
            feature: "snapshots_disk",
        })
    }

    async fn snapshot_restore(&self, _label: &str, _path: &Path) -> Result<(), BackendError> {
        Err(BackendError::Unsupported {
            feature: "snapshots_memory",
        })
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
