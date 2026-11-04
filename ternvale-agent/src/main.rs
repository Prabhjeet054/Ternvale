//! `ternvale-agent` binary. Runs in the guest (Linux init or a Windows
//! service) and logs to stderr.

use std::process::ExitCode;
use std::sync::atomic::AtomicBool;

use ternvale_agent::{run, AgentConfig, Exit, SystemActions, Target};
use ternvale_agent_proto::{AGENT_PORT, AGENT_SERIAL_NAME};

const USAGE: &str = "\
usage: ternvale-agent [--cid N] [--port N] [--uds PATH] [--serial [NAME]] [--auto]
                      [--service] [--install]

Connects to the Ternvale host agent server and serves its requests,
reconnecting with backoff. Default on Unix: vsock CID 2 port 5000.
Default on Windows: auto (vsock, then virtio-serial org.ternvale.agent).
  --cid N         vsock context id (default 2)
  --port N        vsock port (default 5000)
  --uds PATH      Unix socket (host tests)
  --serial [NAME] virtio-console named port (default org.ternvale.agent)
  --auto          vsock then serial
  --service       Windows: Service Control Manager entry
  --install       Windows: register auto-start service with restart on failure
Log level: TERNVALE_AGENT_LOG (default info).";

#[derive(Debug)]
enum Command {
    Help,
    Run(Target),
    Service,
    Install,
}

fn main() -> ExitCode {
    init_logging();
    let command = match parse(std::env::args().skip(1)) {
        Ok(Command::Help) => {
            println!("{USAGE}");
            return ExitCode::SUCCESS;
        }
        Ok(command) => command,
        Err(problem) => {
            tracing::error!(target: "ternvale::agent", problem = %problem, "bad arguments");
            eprintln!("ternvale-agent: {problem}\n{USAGE}");
            return ExitCode::from(2);
        }
    };
    match command {
        Command::Help => unreachable!(),
        Command::Install => install(),
        Command::Service => service(),
        Command::Run(target) => run_foreground(target),
    }
}

fn run_foreground(target: Target) -> ExitCode {
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

fn install() -> ExitCode {
    #[cfg(windows)]
    {
        match ternvale_agent::service::install() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                tracing::error!(target: "ternvale::agent", error = %error, "service install failed");
                eprintln!("ternvale-agent: {error}");
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("ternvale-agent: --install is only supported on Windows");
        ExitCode::from(2)
    }
}

fn service() -> ExitCode {
    #[cfg(windows)]
    {
        match ternvale_agent::service::dispatch() {
            Ok(()) => ExitCode::SUCCESS,
            Err(error) => {
                tracing::error!(target: "ternvale::agent", error = %error, "service dispatch failed");
                eprintln!("ternvale-agent: {error}");
                ExitCode::FAILURE
            }
        }
    }
    #[cfg(not(windows))]
    {
        eprintln!("ternvale-agent: --service is only supported on Windows");
        ExitCode::from(2)
    }
}

fn parse(args: impl Iterator<Item = String>) -> Result<Command, String> {
    let mut args = args.peekable();
    let (mut cid, mut port) = (2u32, AGENT_PORT);
    let mut uds = None;
    let mut serial = None;
    let mut auto = false;
    let mut service = false;
    let mut install = false;
    while let Some(flag) = args.next() {
        let mut value = |name: &str| args.next().ok_or(format!("{name} needs a value"));
        match flag.as_str() {
            "-h" | "--help" => return Ok(Command::Help),
            "--cid" => cid = number(&value("--cid")?)?,
            "--port" => port = number(&value("--port")?)?,
            "--uds" => uds = Some(value("--uds")?),
            "--serial" => {
                serial = Some(match args.peek() {
                    Some(next) if !next.starts_with('-') => args.next().unwrap_or_default(),
                    _ => AGENT_SERIAL_NAME.into(),
                });
            }
            "--auto" => auto = true,
            "--service" => service = true,
            "--install" => install = true,
            other => return Err(format!("unknown argument {other:?}")),
        }
    }
    if install {
        return Ok(Command::Install);
    }
    if service {
        return Ok(Command::Service);
    }
    Ok(Command::Run(target(cid, port, uds, serial, auto)?))
}

fn target(
    cid: u32,
    port: u32,
    uds: Option<String>,
    serial: Option<String>,
    auto: bool,
) -> Result<Target, String> {
    if let Some(path) = uds {
        #[cfg(unix)]
        {
            let _ = (serial, auto);
            return Ok(Target::Unix(path.into()));
        }
        #[cfg(not(unix))]
        {
            let _ = path;
            return Err("--uds is only supported on Unix".into());
        }
    }
    if auto {
        return Ok(Target::Auto {
            cid,
            port,
            serial: serial.unwrap_or_else(|| AGENT_SERIAL_NAME.into()),
        });
    }
    if let Some(name) = serial {
        return Ok(Target::Serial { name });
    }
    Ok(default_target(cid, port))
}

fn default_target(cid: u32, port: u32) -> Target {
    #[cfg(windows)]
    {
        Target::Auto {
            cid,
            port,
            serial: AGENT_SERIAL_NAME.into(),
        }
    }
    #[cfg(not(windows))]
    {
        Target::Vsock { cid, port }
    }
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

    fn args(list: &[&str]) -> Result<Command, String> {
        parse(list.iter().map(|arg| arg.to_string()))
    }

    fn run_target(list: &[&str]) -> Result<Target, String> {
        match args(list)? {
            Command::Run(target) => Ok(target),
            other => Err(format!("expected run, got {other:?}")),
        }
    }

    #[test]
    fn defaults_to_host_cid_and_agent_port() {
        assert_eq!(
            run_target(&[]).unwrap(),
            Target::Vsock { cid: 2, port: 5000 }
        );
        assert_eq!(
            run_target(&["--cid", "3", "--port", "77"]).unwrap(),
            Target::Vsock { cid: 3, port: 77 }
        );
        assert_eq!(
            run_target(&["--uds", "/tmp/a.sock"]).unwrap(),
            Target::Unix("/tmp/a.sock".into())
        );
        assert!(matches!(args(&["--help"]).unwrap(), Command::Help));
    }

    #[test]
    fn serial_and_auto_flags() {
        assert_eq!(
            run_target(&["--serial"]).unwrap(),
            Target::Serial {
                name: AGENT_SERIAL_NAME.into()
            }
        );
        assert_eq!(
            run_target(&["--serial", "org.example.port"]).unwrap(),
            Target::Serial {
                name: "org.example.port".into()
            }
        );
        assert_eq!(
            run_target(&["--auto"]).unwrap(),
            Target::Auto {
                cid: 2,
                port: 5000,
                serial: AGENT_SERIAL_NAME.into(),
            }
        );
    }

    #[test]
    fn bad_arguments_are_explained() {
        assert_eq!(args(&["--port"]).unwrap_err(), "--port needs a value");
        assert!(args(&["--cid", "x"])
            .unwrap_err()
            .starts_with("\"x\" is not a number"));
        assert_eq!(
            args(&["--bogus"]).unwrap_err(),
            "unknown argument \"--bogus\""
        );
    }
}
