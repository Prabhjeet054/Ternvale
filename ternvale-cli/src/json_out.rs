//! `--json` helper for commands that are not control-socket clients.

use anyhow::{Context, Result};
use serde::Serialize;

/// Print `value` as one JSON object when `json`, otherwise run `human`.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn emit(json: bool, value: impl Serialize, human: impl FnOnce()) -> Result<()> {
    if json {
        println!(
            "{}",
            serde_json::to_string(&value).context("encode --json output")?
        );
    } else {
        human();
    }
    Ok(())
}
