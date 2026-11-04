//! launchd / SMAppService helper that owns a shared-mode vmnet interface.

use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;

use ternvale_hv::SharedInterface;
use ternvale_log::LogConfig;
use ternvale_net::{serve, Port, PortInfo, SYSTEM_SOCK};

struct HvPort {
    inner: SharedInterface,
}

impl Port for HvPort {
    fn info(&self) -> PortInfo {
        let info = self.inner.info();
        PortInfo {
            mtu: info.mtu,
            max_packet: info.max_packet,
            max_batch: info.max_write.min(info.max_read),
            mac: info.mac,
        }
    }

    fn write_frames(&mut self, frames: &[&[u8]]) -> std::io::Result<u32> {
        self.inner.write_batch(frames)
    }

    fn read_frames(&mut self) -> std::io::Result<Vec<Vec<u8>>> {
        self.inner.read_batch()
    }

    fn packets_ready(&self) -> bool {
        self.inner.packets_ready()
    }
}

fn main() -> ExitCode {
    let log_dir = LogConfig::default_log_dir().unwrap_or_else(|_| PathBuf::from("/tmp"));
    let _guard = match ternvale_log::init(LogConfig::new("vmnetd", log_dir)) {
        Ok(guard) => guard,
        Err(error) => {
            eprintln!("ternvale-vmnetd: log init: {error}");
            return ExitCode::FAILURE;
        }
    };
    let path = std::env::var("TERNVALE_VMNET_SOCK").unwrap_or_else(|_| SYSTEM_SOCK.into());
    tracing::info!(target: "ternvale::net::vmnet", path = %path, "ternvale-vmnetd starting");
    match run(&path) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            tracing::error!(target: "ternvale::net::vmnet", error = %error, "ternvale-vmnetd failed");
            eprintln!("ternvale-vmnetd: {error}");
            ExitCode::FAILURE
        }
    }
}

#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
fn run(path: &str) -> Result<(), Box<dyn std::error::Error>> {
    let iface = SharedInterface::start_shared()?;
    let listener = bind(path)?;
    serve(listener, HvPort { inner: iface })?;
    Ok(())
}

#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
fn bind(path: &str) -> std::io::Result<UnixListener> {
    if let (Ok(fds), Ok(pid)) = (std::env::var("LISTEN_FDS"), std::env::var("LISTEN_PID")) {
        let n = fds.parse::<u32>().unwrap_or(0);
        let listen_pid = pid.parse::<u32>().unwrap_or(0);
        if n >= 1 && listen_pid == std::process::id() {
            // SAFETY: launchd socket activation puts the first listener on fd 3
            // when LISTEN_FDS/LISTEN_PID match this process.
            let listener = unsafe { UnixListener::from_raw_fd(3 as RawFd) };
            tracing::info!(target: "ternvale::net::vmnet", "using launchd LISTEN_FDS");
            return Ok(listener);
        }
    }
    let _ = std::fs::remove_file(path);
    if let Some(parent) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(parent)?;
    }
    UnixListener::bind(path)
}
