//! `ternvale balloon <name> --target-mib N` / `--auto`.

use std::process::ExitCode;

use anyhow::{bail, Context, Result};

use crate::client;
use crate::commands;
use crate::paths;
use crate::protocol::{Request, Response};

/// `ternvale balloon <name>`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cli",
    skip_all,
    fields(vm = name, target_mib, auto, json)
)]
pub fn balloon(name: &str, target_mib: Option<u64>, auto: bool, json: bool) -> Result<ExitCode> {
    if auto == target_mib.is_some() {
        bail!("pass exactly one of --target-mib or --auto");
    }
    let socket = paths::socket_path(name)?;
    let request = if auto {
        Request::BalloonSet {
            target_mib: None,
            auto: Some(true),
        }
    } else {
        Request::BalloonSet {
            target_mib,
            auto: None,
        }
    };
    let stream = match client::connect(&socket)? {
        Ok(stream) => stream,
        Err(reach) => {
            println!("vm {name}: not running{}", stale(&reach, &socket));
            return Ok(ExitCode::FAILURE);
        }
    };
    let response = client::exchange(stream, &request)?;
    if json {
        println!(
            "{}",
            serde_json::to_string(&response).context("encode balloon response")?
        );
    } else {
        print_human(name, &response)?;
    }
    Ok(if response.ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    })
}

fn stale(reach: &client::Reach, socket: &std::path::Path) -> String {
    match reach {
        client::Reach::NotRunning => String::new(),
        client::Reach::Stale => format!(" (stale socket {})", socket.display()),
    }
}

fn print_human(name: &str, response: &Response) -> Result<()> {
    if !response.ok {
        bail!(
            "balloon-set failed: {}",
            response.error.as_deref().unwrap_or("unknown error")
        );
    }
    let status = response
        .status
        .as_ref()
        .with_context(|| format!("vm {name} answered without a status"))?;
    print!("{}", commands::render(status, response.stats.as_ref()));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::balloon;
    use anyhow::Result;
    use std::process::ExitCode;

    #[test]
    fn rejects_missing_target_and_auto() {
        let err = balloon("nope", None, false, false).expect_err("need flag");
        assert!(err.to_string().contains("--target-mib"), "{err}");
    }

    #[test]
    fn rejects_both_flags() {
        let err = balloon("nope", Some(64), true, false).expect_err("both");
        assert!(err.to_string().contains("exactly one"), "{err}");
    }

    #[test]
    fn not_running_is_failure() -> Result<()> {
        let code = balloon("no-such-ternvale-vm", Some(64), false, false)?;
        assert_eq!(code, ExitCode::FAILURE);
        Ok(())
    }
}
