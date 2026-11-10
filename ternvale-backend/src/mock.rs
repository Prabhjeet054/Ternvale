//! In-memory backend used by the conformance suite.

use std::path::Path;
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};

use ternvale_vmm::VmState;

use crate::{
    AgentChannel, BackendCapabilities, BackendError, BackendStats, DisplaySurface, InputSink,
    MediaSlot, StopMode, VmBackend,
};

/// Lifecycle-only backend: no hypervisor, no host devices.
pub struct MockBackend {
    name: String,
    cpus: u32,
    caps: BackendCapabilities,
    state: Mutex<VmState>,
    listeners: Mutex<Vec<Sender<VmState>>>,
}

impl MockBackend {
    /// Named mock with `cpus` vCPUs. Capabilities default to Linux-only.
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::backend",
        skip_all,
        fields(name, cpus)
    )]
    pub fn new(name: &str, cpus: u32) -> Self {
        Self {
            name: name.to_string(),
            cpus,
            caps: BackendCapabilities {
                guest_linux: true,
                ..BackendCapabilities::default()
            },
            state: Mutex::new(VmState::Created),
            listeners: Mutex::new(Vec::new()),
        }
    }

    fn set_state(&self, to: VmState) {
        let mut state = crate::lock_state(&self.state);
        *state = to;
        drop(state);
        let mut listeners = crate::lock_listeners(&self.listeners);
        listeners.retain(|tx| tx.send(to).is_ok());
    }

    fn require(&self, allowed: &[VmState], op: &'static str) -> Result<VmState, BackendError> {
        let state = *crate::lock_state(&self.state);
        if allowed.contains(&state) {
            Ok(state)
        } else {
            Err(BackendError::InvalidTransition {
                op,
                state: state.as_str().to_string(),
            })
        }
    }
}

impl VmBackend for MockBackend {
    fn capabilities(&self) -> BackendCapabilities {
        self.caps
    }

    async fn start(&self) -> Result<(), BackendError> {
        match self.require(&[VmState::Created, VmState::Running], "start")? {
            VmState::Created => {
                self.set_state(VmState::Running);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn pause(&self) -> Result<(), BackendError> {
        match self.require(&[VmState::Running, VmState::Paused], "pause")? {
            VmState::Running => {
                self.set_state(VmState::Paused);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn resume(&self) -> Result<(), BackendError> {
        match self.require(&[VmState::Paused, VmState::Running], "resume")? {
            VmState::Paused => {
                self.set_state(VmState::Running);
                Ok(())
            }
            _ => Ok(()),
        }
    }

    async fn stop(&self, _mode: StopMode) -> Result<(), BackendError> {
        self.require(
            &[
                VmState::Created,
                VmState::Running,
                VmState::Paused,
                VmState::Stopping,
            ],
            "stop",
        )?;
        self.set_state(VmState::Stopped);
        Ok(())
    }

    fn state(&self) -> VmState {
        *crate::lock_state(&self.state)
    }

    fn subscribe_state(&self) -> Receiver<VmState> {
        let (tx, rx) = mpsc::channel();
        let _ = tx.send(self.state());
        crate::lock_listeners(&self.listeners).push(tx);
        rx
    }

    async fn query_stats(&self) -> Result<BackendStats, BackendError> {
        Ok(BackendStats {
            name: self.name.clone(),
            state: self.state(),
            cpus: self.cpus,
            cpu_entries: self.cpus as usize,
        })
    }

    async fn attach_media(&self, _slot: MediaSlot, _path: &Path) -> Result<(), BackendError> {
        Ok(())
    }

    async fn detach_media(&self, _slot: MediaSlot) -> Result<(), BackendError> {
        Ok(())
    }

    async fn snapshot_save(&self, _label: &str, _path: &Path) -> Result<(), BackendError> {
        Err(BackendError::Unsupported {
            feature: "snapshots_disk",
        })
    }

    async fn snapshot_restore(&self, _label: &str, _path: &Path) -> Result<(), BackendError> {
        Err(BackendError::Unsupported {
            feature: "snapshots_disk",
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
        Err(BackendError::Unsupported {
            feature: "agent_channel",
        })
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
