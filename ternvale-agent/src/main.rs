//! `ternvale-agent` binary. Runs in the guest (started by init) and logs to
//! stderr, which is the guest console.

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use ternvale_agent::{run, AgentConfig, Exit, SystemActions, Target};
use ternvale_agent_proto::AGENT_PORT;

const USAGE: &str = "\
usage: ternvale-agent [--cid N] [--port N] [--uds PATH]

Connects to the Ternvale host agent server and serves its requests,
reconnecting with backoff. Default: vsock CID 2 (the host), port 5000.
  --cid N      vsock context id to connect to (default 2)
  --port N     vsock port (default 5000)
  --uds PATH   connect to a Unix socket instead of vsock
Log level: TERNVALE_AGENT_LOG (default info), e.g. TERNVALE_AGENT_LOG=debug.";

fn main() -> ExitCode {
    init_logging();
    let target = match parse(std::env::args().skip(1)) {
        Ok(Some(target)) => target,
        Ok(None) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Err(problem) => {
            tracing::error!(target: "ternvale::agent", problem = %problem, "bad arguments");
            eprintln!("ternvale-agent: {problem}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    let stop = AtomicBool::new(false);
    let exit = run(
        &AgentConfig::new(target),
        &mut SystemActions::default(),
        &stop,
    );
    tracing::info!(target: "ternvale::agent", exit = ?exit, "agent exiting");
    match exit {
        Exit::Shutdown | Exit::Stopped => ExitCode::SUCCESS,
    }
}

/// `Ok(None)` for `--help`.
fn parse(mut args: impl Iterator<Item = String>) -> Result<Option<Target>, String> {
    let (mut cid, mut port, mut uds) = (2u32, AGENT_PORT, None);
    while let Some(flag) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match flag.as_str() {
            "-h" | "--help" => return Ok(None),
            "--cid" => cid = number(&value("--cid")?)?,
            "--port" => port = number(&value("--port")?)?,
            "--uds" => uds = Some(value("--uds")?.into()),
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    Ok(Some(match uds {
        Some(path) => Target::Unix(path),
        None => Target::Vsock { cid, port },
    }))
}

fn number(text: &str) -> Result<u32, String> {
    text.parse()
        .map_err(|error| format!("{text:?} is not a number: {error}"))
}

fn init_logging() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("TERNVALE_AGENT_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .with_ansi(false)
        .init();
    std::panic::set_hook(Box::new(|info| {
        let backtrace = std::backtrace::Backtrace::force_capture();
        let location = info
            .location()
            .map(|at| format!("{}:{}", at.file(), at.line()))
            .unwrap_or_default();
        tracing::error!(target: "ternvale::agent", panic = %info, location = %location, backtrace = %backtrace, "agent panicked");
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Result<Option<Target>, String> {
        parse(list.iter().map(|arg| arg.to_string()))
    }

    #[test]
    fn defaults_to_host_cid_and_agent_port() {
        assert_eq!(args(&[]), Ok(Some(Target::Vsock { cid: 2, port: 5000 })));
        assert_eq!(
            args(&["--cid", "3", "--port", "77"]),
            Ok(Some(Target::Vsock { cid: 3, port: 77 }))
        );
        assert_eq!(
            args(&["--uds", "/tmp/a.sock"]),
            Ok(Some(Target::Unix("/tmp/a.sock".into())))
        );
        assert_eq!(args(&["--help"]), Ok(None));
    }

    #[test]
    fn bad_arguments_are_explained() {
        assert_eq!(args(&["--port"]), Err("--port needs a value".into()));
        assert!(args(&["--cid", "x"])
            .expect_err("nan")
            .starts_with("\"x\" is not a number"));
        assert_eq!(
            args(&["--bogus"]),
            Err("unknown argument \"--bogus\"".into())
        );
    }
}
