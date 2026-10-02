//! What the agent does for each host request.

use crate::AgentError;

/// Guest-side effects of host requests. [`SystemActions`] is the real one;
/// tests substitute a recorder.
pub trait Actions {
    /// `SetResolution`.
    fn set_resolution(&mut self, width: u32, height: u32) -> Result<(), AgentError>;
    /// `ClipboardSet`.
    fn clipboard_set(&mut self, text: &str) -> Result<(), AgentError>;
    /// `Shutdown`. On Linux this does not return on success.
    fn shutdown(&mut self) -> Result<(), AgentError>;
}

/// Real actions for a headless Linux guest.
#[derive(Debug, Default)]
pub struct SystemActions {
    clipboard: String,
}

impl SystemActions {
    /// Last clipboard text received.
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn clipboard(&self) -> &str {
        &self.clipboard
    }
}

impl Actions for SystemActions {
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::agent",
        skip_all,
        fields(width, height)
    )]
    fn set_resolution(&mut self, width: u32, height: u32) -> Result<(), AgentError> {
        // TODO(display): apply via DRM/KMS once the guest has a virtio-gpu display.
        tracing::warn!(target: "ternvale::agent", width, height, "no display in this guest; resolution not applied");
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(len = text.len()))]
    fn clipboard_set(&mut self, text: &str) -> Result<(), AgentError> {
        // TODO(display): hand to the guest's clipboard owner (Wayland/X) when there is one.
        self.clipboard = text.to_string();
        tracing::info!(target: "ternvale::agent", len = text.len(), "clipboard updated");
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
    fn shutdown(&mut self) -> Result<(), AgentError> {
        power_off()
    }
}

#[cfg(target_os = "linux")]
fn power_off() -> Result<(), AgentError> {
    use nix::sys::reboot::{reboot, RebootMode};
    tracing::info!(target: "ternvale::agent", "host requested shutdown; syncing and powering off");
    nix::unistd::sync();
    let error = match reboot(RebootMode::RB_POWER_OFF) {
        Ok(never) => match never {},
        Err(errno) => errno,
    };
    tracing::error!(target: "ternvale::agent", error = %error, "power off failed");
    Err(AgentError::Action {
        action: "power off",
        source: std::io::Error::from(error),
    })
}

#[cfg(not(target_os = "linux"))]
fn power_off() -> Result<(), AgentError> {
    tracing::info!(target: "ternvale::agent", "host requested shutdown; not a Linux guest, exiting instead of powering off");
    Ok(())
}
