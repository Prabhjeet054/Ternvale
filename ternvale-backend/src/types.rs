//! Shared backend types (media, stop mode, display, input, agent, stats).

use std::os::unix::net::UnixStream;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use ternvale_vmm::VmState;

use crate::BackendError;

/// How [`crate::VmBackend::stop`] should tear the VM down.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopMode {
    /// Orderly shutdown (`request_stop(false)` then finish).
    Graceful,
    /// Forced stop (`request_stop(true)` then finish).
    Force,
}

/// Optical / installer slots matching the control API `media.attach` enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaSlot {
    Installer,
    Tools,
    Drivers,
    Unattend,
}

impl MediaSlot {
    /// Lower-case slot name.
    #[tracing::instrument(level = "trace", target = "ternvale::backend", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Installer => "installer",
            Self::Tools => "tools",
            Self::Drivers => "drivers",
            Self::Unattend => "unattend",
        }
    }
}

/// Stats shape every backend must fill. `cpu_entries` matches configured vCPUs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendStats {
    pub name: String,
    pub state: VmState,
    pub cpus: u32,
    pub cpu_entries: usize,
}

/// Guest framebuffer handle. Native has no custom pipeline (Step 91).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplaySurface {
    pub width: u32,
    pub height: u32,
}

/// Keyboard / pointer events into the guest.
pub trait InputSink: Send + Sync {
    /// Key press or release (`keycode` is a USB HID usage).
    fn put_key(&self, keycode: u16, pressed: bool) -> Result<(), BackendError>;
    /// Absolute pointer sample.
    fn put_pointer(&self, x: u16, y: u16, buttons: u8) -> Result<(), BackendError>;
}

/// One end of a vsock-like byte stream (the peer stays in the backend).
#[derive(Debug)]
pub struct AgentChannel {
    /// Connected Unix stream.
    pub stream: UnixStream,
}

/// In-memory media attachment recorded by [`crate::NativeBackend`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaAttachment {
    pub slot: MediaSlot,
    pub path: PathBuf,
}
