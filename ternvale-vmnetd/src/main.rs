//! launchd / SMAppService helper that owns vmnet interfaces for the VMM.

use std::os::unix::io::{FromRawFd, RawFd};
use std::os::unix::net::UnixListener;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::{Mutex, OnceLock};
use std::thread;
use std::time::Duration;

use ternvale_hv::{
    list_bridgeable, SharedInterface, StartSpec, VMNET_BRIDGED_MODE, VMNET_HOST_MODE,
    VMNET_SHARED_MODE,
};
use ternvale_log::LogConfig;
use ternvale_net::{serve_client, Open, Port, PortInfo, SYSTEM_SOCK};

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
    if std::env::args().any(|a| a == "--list-bridges") {
        for name in list_bridgeable() {
            println!("{name}");
        }
        return ExitCode::SUCCESS;
    }
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
    let listener = bind(path)?;
    tracing::info!(target: "ternvale::net::vmnet", "vmnet helper listening");
    loop {
        let (stream, _) = listener.accept()?;
        thread::spawn(move || {
            if let Err(error) = serve_client(stream, start_port) {
                tracing::warn!(
                    target: "ternvale::net::vmnet",
                    error = %error,
                    "vmnet helper session ended"
                );
            }
        });
    }
}

fn start_lock() -> &'static Mutex<()> {
    static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    LOCK.get_or_init(|| Mutex::new(()))
}

#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all, fields(mode = open.mode, network = %open.network, bridge = %open.bridge))]
fn start_port(open: Open) -> std::io::Result<HvPort> {
    let spec = spec_from_open(&open)?;
    tracing::info!(
        target: "ternvale::net::vmnet",
        mode = spec.mode,
        network = spec.network.unwrap_or(""),
        bridge = spec.bridge.unwrap_or(""),
        "starting vmnet interface for client"
    );
    let _guard = match start_lock().lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let inner = start_with_retry(spec)?;
    Ok(HvPort { inner })
}

fn start_with_retry(mut spec: StartSpec<'_>) -> std::io::Result<SharedInterface> {
    let mut last = None;
    for attempt in 1..=8u32 {
        match SharedInterface::start(spec) {
            Ok(iface) => return Ok(iface),
            Err(error) => {
                let busy = error.to_string().contains("SHARING_SERVICE_BUSY");
                tracing::warn!(
                    target: "ternvale::net::vmnet",
                    attempt,
                    busy,
                    host_ip = spec.host_ip.unwrap_or(""),
                    error = %error,
                    "vmnet_start_interface retry"
                );
                last = Some(error);
                if busy && spec.host_ip.is_some() {
                    spec.host_ip = None;
                    spec.host_mask = None;
                    tracing::info!(
                        target: "ternvale::net::vmnet",
                        "retrying host-only start without host IP"
                    );
                    continue;
                }
                if !busy {
                    break;
                }
                thread::sleep(Duration::from_millis(150 * u64::from(attempt)));
            }
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("vmnet_start_interface failed with no error")))
}

fn spec_from_open(open: &Open) -> std::io::Result<StartSpec<'_>> {
    match open.mode {
        VMNET_SHARED_MODE => Ok(StartSpec {
            mode: VMNET_SHARED_MODE,
            network: None,
            bridge: None,
            host_ip: None,
            host_mask: None,
        }),
        VMNET_HOST_MODE => {
            let network = if open.network.is_empty() {
                "default"
            } else {
                open.network.as_str()
            };
            Ok(StartSpec {
                mode: VMNET_HOST_MODE,
                network: Some(network),
                bridge: None,
                host_ip: None,
                host_mask: None,
            })
        }
        VMNET_BRIDGED_MODE => {
            if open.bridge.is_empty() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "bridged mode needs bridge_interface",
                ));
            }
            Ok(StartSpec {
                mode: VMNET_BRIDGED_MODE,
                network: None,
                bridge: Some(open.bridge.as_str()),
                host_ip: None,
                host_mask: None,
            })
        }
        other => Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("unknown vmnet mode {other}"),
        )),
    }
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

#[cfg(test)]
mod tests {
    use super::spec_from_open;
    use ternvale_hv::{network_uuid, VMNET_BRIDGED_MODE, VMNET_HOST_MODE, VMNET_SHARED_MODE};
    use ternvale_net::Open;

    #[test]
    fn host_only_uuid_is_stable() {
        assert_eq!(network_uuid("lab"), network_uuid("lab"));
        assert_ne!(network_uuid("lab"), network_uuid("other"));
    }

    #[test]
    fn spec_maps_modes() {
        let shared_open = Open::default();
        let shared = spec_from_open(&shared_open).expect("shared");
        assert_eq!(shared.mode, VMNET_SHARED_MODE);
        let host_open = Open {
            mode: VMNET_HOST_MODE,
            network: "lab".into(),
            bridge: String::new(),
        };
        let host = spec_from_open(&host_open).expect("host");
        assert_eq!(host.mode, VMNET_HOST_MODE);
        assert_eq!(host.network, Some("lab"));
        let bridged_open = Open {
            mode: VMNET_BRIDGED_MODE,
            network: String::new(),
            bridge: "en0".into(),
        };
        let bridged = spec_from_open(&bridged_open).expect("bridged");
        assert_eq!(bridged.bridge, Some("en0"));
        let bad = Open {
            mode: VMNET_BRIDGED_MODE,
            network: String::new(),
            bridge: String::new(),
        };
        assert!(spec_from_open(&bad).is_err());
    }
}
