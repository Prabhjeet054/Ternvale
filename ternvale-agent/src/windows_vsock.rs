//! Windows `viosock` Winsock client (virtio-win).
//!
//! TODO(verify): AF_VSOCK catalog id and `sockaddr_vm` layout against the
//! virtio-win build on the tools ISO. Red Hat virtio-win 0.1.266 (2024)
//! registers AF_VSOCK as 40; confirm before a Windows live test. `viosock`
//! has historically lagged `vioserial`; prefer serial if vsock connect fails.

#![cfg(windows)]

use std::io::{self, Read, Write};
use std::mem::size_of;
use std::time::Duration;

use windows_sys::Win32::Networking::WinSock::{
    closesocket, connect as winsock_connect, recv, send, setsockopt, shutdown as winsock_shutdown,
    socket, WSAStartup, INVALID_SOCKET, SOCKADDR, SOCKET, SOCKET_ERROR, SOCK_STREAM, SOL_SOCKET,
    SO_RCVTIMEO, WSADATA,
};

/// TODO(verify): virtio-win `viosock` Winsock address family.
const AF_VSOCK: u16 = 40;
/// Winsock `SD_BOTH`.
const SD_BOTH: i32 = 2;

#[repr(C)]
struct SockaddrVm {
    svm_family: u16,
    svm_reserved1: u16,
    svm_port: u32,
    svm_cid: u32,
    svm_zero: [u8; 4],
}

/// Connected `viosock` stream.
#[derive(Debug)]
pub struct WinVsock {
    socket: SOCKET,
}

impl WinVsock {
    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        let ms: u32 = timeout
            .map(|t| u32::try_from(t.as_millis()).unwrap_or(u32::MAX))
            .unwrap_or(0);
        // SAFETY: `socket` is a live SOCKET owned by this struct; `ms` lives
        // for the duration of setsockopt.
        let rc = unsafe {
            setsockopt(
                self.socket,
                SOL_SOCKET,
                SO_RCVTIMEO,
                (&raw const ms).cast(),
                size_of::<u32>() as i32,
            )
        };
        if rc == SOCKET_ERROR {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }

    #[tracing::instrument(level = "trace", target = "ternvale::agent", skip_all)]
    pub fn shutdown(&self) -> io::Result<()> {
        // SAFETY: `socket` is still open; shutdown does not transfer ownership.
        let rc = unsafe { winsock_shutdown(self.socket, SD_BOTH) };
        if rc == SOCKET_ERROR {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

impl Drop for WinVsock {
    fn drop(&mut self) {
        // SAFETY: exclusive owner of `socket`; closesocket once.
        unsafe {
            closesocket(self.socket);
        }
    }
}

impl Read for WinVsock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: `buf` is writable for `buf.len()` bytes.
        let n = unsafe { recv(self.socket, buf.as_mut_ptr(), buf.len() as i32, 0) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }
}

impl Write for WinVsock {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        // SAFETY: `buf` is readable for `buf.len()` bytes.
        let n = unsafe { send(self.socket, buf.as_ptr(), buf.len() as i32, 0) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(n as usize)
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(cid, port)
)]
pub fn connect(cid: u32, port: u32) -> io::Result<WinVsock> {
    wsa_once()?;
    // SAFETY: socket() returns a new SOCKET or INVALID_SOCKET.
    let socket = unsafe { socket(i32::from(AF_VSOCK), SOCK_STREAM, 0) };
    if socket == INVALID_SOCKET {
        return Err(io::Error::last_os_error());
    }
    let addr = SockaddrVm {
        svm_family: AF_VSOCK,
        svm_reserved1: 0,
        svm_port: port,
        svm_cid: cid,
        svm_zero: [0; 4],
    };
    // SAFETY: `addr` is a sockaddr_vm the provider expects; length matches.
    let rc = unsafe {
        winsock_connect(
            socket,
            (&raw const addr).cast::<SOCKADDR>(),
            size_of::<SockaddrVm>() as i32,
        )
    };
    if rc == SOCKET_ERROR {
        let err = io::Error::last_os_error();
        unsafe { closesocket(socket) };
        return Err(err);
    }
    tracing::info!(
        target: "ternvale::agent",
        cid,
        port,
        af = AF_VSOCK,
        "connected over viosock"
    );
    Ok(WinVsock { socket })
}

fn wsa_once() -> io::Result<()> {
    use std::sync::OnceLock;
    static START: OnceLock<i32> = OnceLock::new();
    let rc = *START.get_or_init(|| {
        let mut data = unsafe { std::mem::zeroed::<WSADATA>() };
        // SAFETY: WSAStartup writes WSADATA; called once per process.
        unsafe { WSAStartup(0x0202, &mut data) }
    });
    if rc != 0 {
        Err(io::Error::from_raw_os_error(rc))
    } else {
        Ok(())
    }
}
