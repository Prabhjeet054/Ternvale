//! Guest loopback TCP → host IPP vsock (CID 2, port 8631).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::OnceLock;

use ternvale_agent_proto::{IPP_LOCAL_PORT, IPP_PORT};

static STARTED: OnceLock<()> = OnceLock::new();
static STOP: AtomicBool = AtomicBool::new(false);

/// Start the localhost IPP forwarder once per process.
#[tracing::instrument(level = "debug", target = "ternvale::print", skip_all)]
pub fn ensure_running() {
    STARTED.get_or_init(|| {
        let _ = std::thread::Builder::new()
            .name("ipp-fwd".into())
            .spawn(listen);
    });
}

fn listen() {
    let bind = format!("127.0.0.1:{IPP_LOCAL_PORT}");
    let listener = match TcpListener::bind(&bind) {
        Ok(l) => l,
        Err(error) => {
            tracing::warn!(
                target: "ternvale::print",
                error = %error,
                bind = %bind,
                "could not bind guest IPP forwarder"
            );
            return;
        }
    };
    let _ = listener.set_nonblocking(true);
    tracing::info!(
        target: "ternvale::print",
        bind = %bind,
        vsock_port = IPP_PORT,
        "guest IPP forwarder listening"
    );
    while !STOP.load(Ordering::Acquire) {
        match listener.accept() {
            Ok((mut client, _)) => {
                std::thread::spawn(move || {
                    if let Err(error) = forward(&mut client) {
                        tracing::debug!(target: "ternvale::print", error = %error, "ipp forward ended");
                    }
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(error) => {
                tracing::warn!(target: "ternvale::print", error = %error, "ipp forwarder accept");
                std::thread::sleep(std::time::Duration::from_millis(50));
            }
        }
    }
}

fn forward(client: &mut std::net::TcpStream) -> Result<(), String> {
    let mut host = connect_host().map_err(|e| format!("connect host IPP: {e}"))?;
    let mut down = host
        .try_clone()
        .map_err(|e| format!("clone host IPP: {e}"))?;
    let mut guest = client
        .try_clone()
        .map_err(|e| format!("clone guest IPP: {e}"))?;
    std::thread::spawn(move || {
        let _ = copy_limited(&mut down, &mut guest);
    });
    copy_limited(client, &mut host)
}

fn copy_limited(src: &mut dyn Read, dst: &mut dyn Write) -> Result<(), String> {
    let mut buf = [0u8; 8192];
    let mut total = 0usize;
    loop {
        let n = src.read(&mut buf).map_err(|e| e.to_string())?;
        if n == 0 {
            break;
        }
        total = total.saturating_add(n);
        if total > 32 * 1024 * 1024 {
            return Err("ipp forward exceeded 32 MiB".into());
        }
        dst.write_all(&buf[..n]).map_err(|e| e.to_string())?;
    }
    Ok(())
}

fn connect_host() -> std::io::Result<HostStream> {
    if let Some(path) = std::env::var_os("TERNVALE_IPP_UDS") {
        return UnixWrap::connect(path.as_ref()).map(HostStream::Unix);
    }
    #[cfg(target_os = "linux")]
    {
        let stream = vsock::VsockStream::connect_with_cid_port(2, IPP_PORT)?;
        return Ok(HostStream::Vsock(stream));
    }
    #[cfg(windows)]
    {
        let stream = crate::windows_vsock::connect(2, IPP_PORT)?;
        return Ok(HostStream::Vsock(stream));
    }
    #[cfg(not(any(target_os = "linux", windows)))]
    {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "set TERNVALE_IPP_UDS for host tests",
        ))
    }
}

enum HostStream {
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
    #[cfg(target_os = "linux")]
    Vsock(vsock::VsockStream),
    #[cfg(windows)]
    Vsock(crate::windows_vsock::WinVsock),
}

impl Read for HostStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.read(buf),
            #[cfg(target_os = "linux")]
            Self::Vsock(s) => s.read(buf),
            #[cfg(windows)]
            Self::Vsock(s) => s.read(buf),
        }
    }
}

impl Write for HostStream {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.write(buf),
            #[cfg(target_os = "linux")]
            Self::Vsock(s) => s.write(buf),
            #[cfg(windows)]
            Self::Vsock(s) => s.write(buf),
        }
    }
    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.flush(),
            #[cfg(target_os = "linux")]
            Self::Vsock(s) => s.flush(),
            #[cfg(windows)]
            Self::Vsock(s) => s.flush(),
        }
    }
}

impl HostStream {
    fn try_clone(&self) -> std::io::Result<Self> {
        match self {
            #[cfg(unix)]
            Self::Unix(s) => s.try_clone().map(Self::Unix),
            #[cfg(target_os = "linux")]
            Self::Vsock(s) => s.try_clone().map(Self::Vsock),
            #[cfg(windows)]
            Self::Vsock(_) => Err(std::io::Error::other("win vsock clone unsupported")),
        }
    }
}

#[cfg(unix)]
struct UnixWrap;
#[cfg(unix)]
impl UnixWrap {
    fn connect(path: &std::path::Path) -> std::io::Result<std::os::unix::net::UnixStream> {
        std::os::unix::net::UnixStream::connect(path)
    }
}
