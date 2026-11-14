//! [`NativeBackend`]: the existing [`VmControl`] orchestrator, unchanged.

use std::collections::HashMap;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ternvale_config::VmConfig;
use ternvale_vmm::{ExitReason, VmControl, VmState};

use crate::{
    AgentChannel, BackendCapabilities, BackendError, BackendStats, DisplaySurface, InputSink,
    MediaSlot, StopMode, VmBackend,
};

const PAUSE_TIMEOUT: Duration = Duration::from_secs(5);

/// Hypervisor.framework backend. Lifecycle calls go through [`VmControl`].
pub struct NativeBackend {
    config: VmConfig,
    control: Arc<VmControl>,
    media: Mutex<HashMap<MediaSlot, std::path::PathBuf>>,
    listeners: Mutex<Vec<Sender<VmState>>>,
    agent_peer: Mutex<Option<UnixStream>>,
}

impl NativeBackend {
    /// Wrap a new controller for `config.name` / `config.cpus`.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::backend",
        skip_all,
        fields(vm = %config.name, cpus = config.cpus)
    )]
    pub async fn create(config: VmConfig) -> Result<Self, BackendError> {
        let control = Arc::new(VmControl::new(&config.name, config.cpus));
        tracing::info!(
            target: "ternvale::backend",
            vm = %config.name,
            cpus = config.cpus,
            "native backend created"
        );
        Ok(Self {
            config,
            control,
            media: Mutex::new(HashMap::new()),
            listeners: Mutex::new(Vec::new()),
            agent_peer: Mutex::new(None),
        })
    }

    /// The wrapped orchestrator (same object the control socket already uses).
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    pub fn control(&self) -> &Arc<VmControl> {
        &self.control
    }

    fn emit(&self, state: VmState) {
        let mut listeners = crate::lock_listeners(&self.listeners);
        listeners.retain(|tx| tx.send(state).is_ok());
    }

    fn map_ctrl<T>(result: Result<T, ternvale_vmm::ControlError>) -> Result<T, BackendError> {
        result.map_err(BackendError::from_control)
    }
}

impl VmBackend for NativeBackend {
    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all)]
    fn capabilities(&self) -> BackendCapabilities {
        BackendCapabilities::native()
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    async fn start(&self) -> Result<(), BackendError> {
        match self.control.state() {
            VmState::Created => {
                self.control.mark_running();
                self.emit(self.control.state());
                tracing::info!(
                    target: "ternvale::backend",
                    vm = %self.config.name,
                    "native backend started"
                );
                Ok(())
            }
            VmState::Running => Ok(()),
            state => Err(BackendError::InvalidTransition {
                op: "start",
                state: state.as_str().to_string(),
            }),
        }
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    async fn pause(&self) -> Result<(), BackendError> {
        Self::map_ctrl(self.control.pause(PAUSE_TIMEOUT))?;
        self.emit(self.control.state());
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    async fn resume(&self) -> Result<(), BackendError> {
        Self::map_ctrl(self.control.resume())?;
        self.emit(self.control.state());
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name, ?mode))]
    async fn stop(&self, mode: StopMode) -> Result<(), BackendError> {
        let force = matches!(mode, StopMode::Force);
        Self::map_ctrl(self.control.request_stop(force))?;
        self.control.finish(Ok(ExitReason::SystemOff));
        self.emit(self.control.state());
        tracing::info!(
            target: "ternvale::backend",
            vm = %self.config.name,
            force,
            state = %self.control.state(),
            "native backend stopped"
        );
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    fn state(&self) -> VmState {
        self.control.state()
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    fn subscribe_state(&self) -> Receiver<VmState> {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(self.control.state());
        crate::lock_listeners(&self.listeners).push(tx);
        rx
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    async fn query_stats(&self) -> Result<BackendStats, BackendError> {
        let stats = self.control.stats();
        Ok(BackendStats {
            name: stats.status.name,
            state: stats.status.state,
            cpus: stats.status.cpus,
            cpu_entries: stats.cpus.len(),
        })
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name, slot = %slot.as_str(), path = %path.display()))]
    async fn attach_media(&self, slot: MediaSlot, path: &Path) -> Result<(), BackendError> {
        crate::lock_media(&self.media).insert(slot, path.to_path_buf());
        tracing::info!(
            target: "ternvale::backend",
            vm = %self.config.name,
            slot = %slot.as_str(),
            path = %path.display(),
            "media attached"
        );
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name, slot = %slot.as_str()))]
    async fn detach_media(&self, slot: MediaSlot) -> Result<(), BackendError> {
        crate::lock_media(&self.media).remove(&slot);
        tracing::info!(
            target: "ternvale::backend",
            vm = %self.config.name,
            slot = %slot.as_str(),
            "media detached"
        );
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name, label))]
    async fn snapshot_save(&self, label: &str, path: &Path) -> Result<(), BackendError> {
        let _ = (label, path);
        Err(BackendError::from_control(
            ternvale_vmm::ControlError::SnapshotHostMissing,
        ))
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name, label))]
    async fn snapshot_restore(&self, label: &str, path: &Path) -> Result<(), BackendError> {
        let _ = (label, path);
        Err(BackendError::from_control(
            ternvale_vmm::ControlError::SnapshotHostMissing,
        ))
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    async fn display_surface(&self) -> Result<DisplaySurface, BackendError> {
        // Frames go through the CLI presenter socket / app Metal viewer, not
        // this trait. `custom_display_pipeline` is still true so the app keeps
        // VMMetalDisplayView for native Linux/Windows guests.
        Err(BackendError::Unsupported {
            feature: "display_surface_trait",
        })
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    fn input_sink(&self) -> Arc<dyn InputSink> {
        Arc::new(NativeInput {
            vm: self.config.name.clone(),
        })
    }

    #[tracing::instrument(level = "debug", target = "ternvale::backend", skip_all, fields(vm = %self.config.name))]
    async fn agent_channel(&self) -> Result<AgentChannel, BackendError> {
        let (a, b) = UnixStream::pair().map_err(|error| BackendError::Io {
            op: "agent_channel",
            detail: error.to_string(),
        })?;
        *crate::lock_agent(&self.agent_peer) = Some(b);
        tracing::debug!(target: "ternvale::backend", vm = %self.config.name, "agent channel pair created");
        Ok(AgentChannel { stream: a })
    }
}

struct NativeInput {
    vm: String,
}

impl InputSink for NativeInput {
    #[tracing::instrument(level = "trace", target = "ternvale::backend", skip_all, fields(vm = %self.vm, keycode, pressed))]
    fn put_key(&self, keycode: u16, pressed: bool) -> Result<(), BackendError> {
        tracing::trace!(
            target: "ternvale::backend",
            vm = %self.vm,
            keycode,
            pressed,
            "native input key"
        );
        Ok(())
    }

    #[tracing::instrument(level = "trace", target = "ternvale::backend", skip_all, fields(vm = %self.vm, x, y, buttons))]
    fn put_pointer(&self, x: u16, y: u16, buttons: u8) -> Result<(), BackendError> {
        tracing::trace!(
            target: "ternvale::backend",
            vm = %self.vm,
            x,
            y,
            buttons,
            "native input pointer"
        );
        Ok(())
    }
}
