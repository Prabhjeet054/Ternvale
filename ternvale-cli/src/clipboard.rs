//! macOS pasteboard helpers for guest clipboard sync.

use std::time::Duration;

use anyhow::{Context, Result};
use ternvale_agent_proto::ClipboardText;
use ternvale_devices::{AgentServer, AgentServerError};

/// Default wait for guest `ClipboardGet`.
pub const DEFAULT_CLIPBOARD_TIMEOUT: Duration = Duration::from_secs(5);

/// Read plain text from the host pasteboard.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn host_text() -> Result<String> {
    let mut board = arboard::Clipboard::new().context("open the host pasteboard")?;
    board
        .get_text()
        .context("read text from the host pasteboard")
}

/// Write plain text to the host pasteboard.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(len = text.len()))]
pub fn set_host_text(text: &str) -> Result<()> {
    let mut board = arboard::Clipboard::new().context("open the host pasteboard")?;
    board
        .set_text(text.to_owned())
        .context("write text to the host pasteboard")?;
    tracing::info!(target: "ternvale::cli", "host pasteboard updated");
    Ok(())
}

/// Push host pasteboard text to the guest agent.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all, fields(len = text.len()))]
pub fn push_to_guest(agent: &AgentServer, text: &str) -> Result<(), AgentServerError> {
    agent.send(&ternvale_agent_proto::Message::ClipboardSet {
        text: ClipboardText(text.to_owned()),
    })
}

/// Pull guest clipboard text, update the host pasteboard, and return it.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn pull_from_guest(agent: &AgentServer, timeout: Duration) -> Result<String> {
    let text = agent
        .clipboard_get(timeout)
        .map_err(|error| anyhow::anyhow!("read clipboard from the guest agent: {error}"))?;
    set_host_text(&text).context("copy guest clipboard to the host pasteboard")?;
    Ok(text)
}
