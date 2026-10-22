//! What the agent does for each host request.

use crate::AgentError;

#[cfg(target_os = "linux")]
const HARNESS_HOST_CLIP_OUT: &str = "/tmp/ternvale-host-clipboard-out";
#[cfg(target_os = "linux")]
const HARNESS_GUEST_CLIP_IN: &str = "/tmp/ternvale-guest-clipboard";

#[cfg(target_os = "linux")]
fn harness_enabled() -> bool {
    std::env::var_os("TERNVALE_AGENT_HARNESS").is_some_and(|v| !v.is_empty())
}

#[cfg(target_os = "linux")]
fn harness_write_host_clip(text: &str) {
    if !harness_enabled() {
        return;
    }
    match std::fs::write(HARNESS_HOST_CLIP_OUT, text) {
        Ok(()) => {
            tracing::info!(
                target: "ternvale::agent",
                path = HARNESS_HOST_CLIP_OUT,
                len = text.len(),
                "harness wrote host clipboard for guest terminal"
            );
        }
        Err(source) => {
            tracing::warn!(
                target: "ternvale::agent",
                path = HARNESS_HOST_CLIP_OUT,
                error = %source,
                "harness could not write host clipboard file"
            );
        }
    }
}

#[cfg(target_os = "linux")]
fn harness_read_guest_clip() -> Option<String> {
    if !harness_enabled() {
        return None;
    }
    match std::fs::read_to_string(HARNESS_GUEST_CLIP_IN) {
        Ok(text) => {
            tracing::info!(
                target: "ternvale::agent",
                path = HARNESS_GUEST_CLIP_IN,
                len = text.len(),
                "harness read guest clipboard file"
            );
            Some(text)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => None,
        Err(source) => {
            tracing::warn!(
                target: "ternvale::agent",
                path = HARNESS_GUEST_CLIP_IN,
                error = %source,
                "harness could not read guest clipboard file"
            );
            None
        }
    }
}

/// Guest-side effects of host requests. [`SystemActions`] is the real one;
/// tests substitute a recorder.
pub trait Actions {
    /// `SetResolution`.
    fn set_resolution(&mut self, width: u32, height: u32) -> Result<(), AgentError>;
    /// `ClipboardSet`.
    fn clipboard_set(&mut self, text: &str) -> Result<(), AgentError>;
    /// `ClipboardGet`.
    fn clipboard_get(&mut self) -> Result<String, AgentError>;
    /// `ShareMount` (virtio-9p on Linux).
    fn mount_share(&mut self, tag: &str, mount_point: &str) -> Result<(), AgentError>;
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
        self.clipboard = text.to_string();
        #[cfg(target_os = "linux")]
        if let Ok(mut child) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard"])
            .stdin(std::process::Stdio::piped())
            .spawn()
        {
            use std::io::Write;
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            let _ = child.wait();
        }
        tracing::info!(target: "ternvale::agent", len = text.len(), "clipboard updated");
        #[cfg(target_os = "linux")]
        harness_write_host_clip(text);
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
    fn clipboard_get(&mut self) -> Result<String, AgentError> {
        #[cfg(target_os = "linux")]
        if let Some(text) = harness_read_guest_clip() {
            return Ok(text);
        }
        #[cfg(target_os = "linux")]
        if let Ok(out) = std::process::Command::new("xclip")
            .args(["-selection", "clipboard", "-o"])
            .output()
        {
            if out.status.success() {
                let text = String::from_utf8_lossy(&out.stdout).into_owned();
                tracing::debug!(target: "ternvale::agent", len = text.len(), "clipboard read from xclip");
                return Ok(text);
            }
        }
        tracing::debug!(target: "ternvale::agent", len = self.clipboard.len(), "clipboard read from agent buffer");
        Ok(self.clipboard.clone())
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::agent",
        skip_all,
        fields(tag, mount_point)
    )]
    fn mount_share(&mut self, tag: &str, mount_point: &str) -> Result<(), AgentError> {
        #[cfg(target_os = "linux")]
        {
            std::fs::create_dir_all(mount_point).map_err(|source| AgentError::Action {
                action: "mkdir mount_point",
                source,
            })?;
            let options = format!("trans=virtio,version=9p2000.L,msize=8192");
            let status = std::process::Command::new("mount")
                .args(["-t", "9p", "-o", &options, tag, mount_point])
                .status()
                .map_err(|source| AgentError::Action {
                    action: "mount 9p",
                    source,
                })?;
            if !status.success() {
                tracing::warn!(target: "ternvale::agent", tag, mount_point, "mount 9p failed");
                return Err(AgentError::Action {
                    action: "mount 9p",
                    source: std::io::Error::from_raw_os_error(status.code().unwrap_or(-1)),
                });
            }
            tracing::info!(target: "ternvale::agent", tag, mount_point, "shared folder mounted");
            return Ok(());
        }
        #[cfg(not(target_os = "linux"))]
        {
            tracing::warn!(target: "ternvale::agent", tag, mount_point, "share_mount ignored on non-Linux guest");
            Ok(())
        }
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
