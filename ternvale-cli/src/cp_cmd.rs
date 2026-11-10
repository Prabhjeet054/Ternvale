//! `ternvale cp <src> <dst>` host ↔ guest via the agent.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use crate::client;
use crate::paths;
use crate::protocol::Request;

/// One side of a copy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// Host path.
    Host(PathBuf),
    /// `vm:/absolute/guest/path`
    Guest {
        /// VM name.
        name: String,
        /// Absolute guest path.
        path: String,
    },
}

/// Parse `name:/path` (VM names longer than one character) vs a host path.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn parse_location(text: &str) -> Location {
    if let Some((name, path)) = text.split_once(':') {
        let drive = name.len() == 1 && name.chars().all(|c| c.is_ascii_alphabetic());
        let vm = name.len() > 1
            && name
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
        if vm && !drive && (path.starts_with('/') || path.starts_with('\\')) {
            return Location::Guest {
                name: name.to_string(),
                path: path.to_string(),
            };
        }
    }
    Location::Host(PathBuf::from(text))
}

/// Copy between host and guest. Resume when the destination is a shorter prefix.
#[tracing::instrument(level = "debug", target = "ternvale::cli", skip_all)]
pub fn cp(src: &str, dst: &str, json: bool) -> Result<ExitCode> {
    let from = parse_location(src);
    let to = parse_location(dst);
    let (name, guest_path, host_path, to_guest) = match (from, to) {
        (Location::Host(host), Location::Guest { name, path }) => (name, path, host, true),
        (Location::Guest { name, path }, Location::Host(host)) => (name, path, host, false),
        (Location::Host(_), Location::Host(_)) => {
            bail!("cp needs one host path and one guest path (vm:/path)")
        }
        (Location::Guest { name: a, .. }, Location::Guest { name: b, .. }) => {
            bail!("cp cannot copy between guests {a} and {b} in one step")
        }
    };
    let mut offset = resume_offset(to_guest, &host_path, &guest_path, &name)?;
    let socket = paths::socket_path(&name)?;
    const SLICE: u64 = 4 * 1024 * 1024;
    loop {
        tracing::debug!(
            target: "ternvale::cli",
            vm = %name,
            offset = ?offset,
            slice = SLICE,
            to_guest,
            "copy slice"
        );
        let request = Request::Copy {
            guest_path: guest_path.clone(),
            host_path: host_path.display().to_string(),
            to_guest,
            offset,
            limit: Some(SLICE),
        };
        let response = client::request(&socket, &name, &request)?;
        if json {
            println!(
                "{}",
                serde_json::to_string(&response).context("encode copy response")?
            );
        }
        if !response.ok {
            let error = response.error.as_deref().unwrap_or("copy failed");
            bail!("vm {name} refused copy: {error}");
        }
        if response.sha256.as_ref().is_some_and(|h| h.len() == 64) {
            if !json {
                let bytes = response.bytes.unwrap_or(0);
                let hash = response.sha256.as_deref().unwrap_or("-");
                let resumed = response.resumed.unwrap_or(false);
                println!("copied {bytes} bytes sha256={hash} resumed={resumed}");
            }
            return Ok(ExitCode::SUCCESS);
        }
        let bytes = response.bytes.unwrap_or(0);
        if Some(bytes) == offset {
            bail!("copy made no progress at offset {bytes}");
        }
        offset = Some(bytes);
    }
}

fn resume_offset(
    to_guest: bool,
    host_path: &Path,
    _guest_path: &str,
    _name: &str,
) -> Result<Option<u64>> {
    if to_guest {
        return Ok(None);
    }
    if host_path.is_file() {
        let len = host_path
            .metadata()
            .with_context(|| format!("stat {}", host_path.display()))?
            .len();
        if len > 0 {
            return Ok(Some(len));
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_guest_and_host_and_windows_drive() {
        assert_eq!(
            parse_location("win11:/tmp/a"),
            Location::Guest {
                name: "win11".into(),
                path: "/tmp/a".into()
            }
        );
        assert_eq!(
            parse_location("/tmp/a"),
            Location::Host(PathBuf::from("/tmp/a"))
        );
        match parse_location("C:\\Windows\\file") {
            Location::Host(_) => {}
            other => panic!("{other:?}"),
        }
    }
}
