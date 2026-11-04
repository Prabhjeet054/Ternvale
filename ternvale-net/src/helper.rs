//! Privileged helper session: one vmnet port, one Unix-socket client.

use std::io::{self, Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::time::Duration;

use super::protocol::{self, write_error, Hello, Msg, Open};

/// What the helper needs from vmnet (or a test fake).
pub trait Port {
    fn info(&self) -> PortInfo;
    fn write_frames(&mut self, frames: &[&[u8]]) -> io::Result<u32>;
    fn read_frames(&mut self) -> io::Result<Vec<Vec<u8>>>;
    fn packets_ready(&self) -> bool;
}

/// Subset of vmnet interface_param used on the wire.
#[derive(Debug, Clone, Copy)]
pub struct PortInfo {
    pub mtu: u32,
    pub max_packet: u32,
    pub max_batch: u32,
    pub mac: [u8; 6],
}

/// Read the client's `TVNO` request, or default to shared after a short wait.
#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
pub fn read_open(stream: &mut UnixStream) -> io::Result<Open> {
    stream.set_read_timeout(Some(Duration::from_millis(250)))?;
    let open = match Open::read_from(&mut *stream) {
        Ok(open) => open,
        Err(error)
            if error.kind() == io::ErrorKind::TimedOut
                || error.kind() == io::ErrorKind::WouldBlock
                || error.kind() == io::ErrorKind::Interrupted =>
        {
            tracing::debug!(
                target: "ternvale::net::vmnet",
                "no open request; defaulting to shared NAT"
            );
            Open::default()
        }
        Err(error) => {
            let _ = stream.set_read_timeout(None);
            return Err(error);
        }
    };
    stream.set_read_timeout(None)?;
    tracing::debug!(
        target: "ternvale::net::vmnet",
        mode = open.mode,
        network = %open.network,
        bridge = %open.bridge,
        "vmnet open request"
    );
    Ok(open)
}

/// Accept one client at a time and move frames (tests / single-port helpers).
#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
pub fn serve(listener: UnixListener, mut port: impl Port) -> io::Result<()> {
    tracing::info!(target: "ternvale::net::vmnet", "vmnet helper listening");
    loop {
        let (mut stream, _) = listener.accept()?;
        let _ = read_open(&mut stream);
        if let Err(error) = session(stream, &mut port) {
            tracing::warn!(
                target: "ternvale::net::vmnet",
                error = %error,
                "vmnet helper session ended"
            );
        }
    }
}

/// One client: start a port from `Open`, or send `TVNE` and return.
#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
pub fn serve_client<P: Port>(
    mut stream: UnixStream,
    start: impl FnOnce(Open) -> io::Result<P>,
) -> io::Result<()> {
    let open = read_open(&mut stream)?;
    match start(open) {
        Ok(mut port) => session(stream, &mut port),
        Err(error) => {
            tracing::error!(
                target: "ternvale::net::vmnet",
                error = %error,
                "vmnet interface start failed"
            );
            write_error(&mut stream, &error.to_string())?;
            let _ = stream.flush();
            Err(error)
        }
    }
}

fn session(mut stream: UnixStream, port: &mut impl Port) -> io::Result<()> {
    let info = port.info();
    let hello = Hello {
        mtu: info.mtu,
        max_packet: info.max_packet,
        max_batch: info.max_batch.max(1),
        mac: info.mac,
    };
    hello.write_to(&mut stream)?;
    Msg::Link(true).write_to(&mut stream)?;
    stream.flush()?;
    tracing::info!(
        target: "ternvale::net::vmnet",
        mtu = hello.mtu,
        max_packet = hello.max_packet,
        "vmnet helper session link up"
    );
    stream.set_nonblocking(true)?;
    let mut buf = Vec::new();
    let mut pending_tx: Vec<Vec<u8>> = Vec::new();
    let mut tmp = [0u8; 8192];
    loop {
        match port.read_frames() {
            Ok(frames) => {
                if !frames.is_empty() {
                    for frame in frames {
                        Msg::Data(frame).write_to(&mut stream)?;
                    }
                    stream.flush()?;
                }
            }
            Err(error) => {
                tracing::warn!(
                    target: "ternvale::net::vmnet",
                    error = %error,
                    reason = "vmnet_read failed",
                    "packet drop"
                );
                Msg::Drop(format!("vmnet_read: {error}")).write_to(&mut stream)?;
            }
        }
        match stream.read(&mut tmp) {
            Ok(0) => {
                tracing::info!(target: "ternvale::net::vmnet", "vmnet helper session link down");
                return Ok(());
            }
            Ok(n) => buf.extend_from_slice(&tmp[..n]),
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                if !pending_tx.is_empty() {
                    flush_tx(port, &mut pending_tx, &mut stream)?;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => return Err(error),
        }
        while let Some(msg) = protocol::take_msg(&mut buf) {
            match msg? {
                Msg::Data(frame) => {
                    if frame.len() as u32 > info.max_packet {
                        tracing::warn!(
                            target: "ternvale::net::vmnet",
                            len = frame.len(),
                            reason = "frame exceeds vmnet max-packet-size",
                            "packet drop"
                        );
                        Msg::Drop("frame exceeds vmnet max-packet-size".into())
                            .write_to(&mut stream)?;
                        continue;
                    }
                    pending_tx.push(frame);
                    if pending_tx.len() as u32 >= info.max_batch.max(1) {
                        flush_tx(port, &mut pending_tx, &mut stream)?;
                    }
                }
                Msg::Link(_) | Msg::Drop(_) => {}
            }
        }
    }
}

fn flush_tx(
    port: &mut impl Port,
    pending: &mut Vec<Vec<u8>>,
    stream: &mut UnixStream,
) -> io::Result<()> {
    if pending.is_empty() {
        return Ok(());
    }
    let refs: Vec<&[u8]> = pending.iter().map(|f| f.as_slice()).collect();
    match port.write_frames(&refs) {
        Ok(_) => pending.clear(),
        Err(error) => {
            tracing::warn!(
                target: "ternvale::net::vmnet",
                error = %error,
                reason = "vmnet_write failed",
                "packet drop"
            );
            Msg::Drop(format!("vmnet_write: {error}")).write_to(stream)?;
            pending.clear();
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{serve, Port, PortInfo};
    use crate::{socket_path, Client, SOCK_ENV};
    use std::io;
    use std::os::unix::net::UnixListener;

    struct Echo {
        rx: Vec<Vec<u8>>,
        info: PortInfo,
    }

    impl Port for Echo {
        fn info(&self) -> PortInfo {
            self.info
        }
        fn write_frames(&mut self, frames: &[&[u8]]) -> io::Result<u32> {
            for f in frames {
                self.rx.push(f.to_vec());
            }
            Ok(frames.len() as u32)
        }
        fn read_frames(&mut self) -> io::Result<Vec<Vec<u8>>> {
            Ok(std::mem::take(&mut self.rx))
        }
        fn packets_ready(&self) -> bool {
            !self.rx.is_empty()
        }
    }

    #[test]
    fn client_round_trips_through_helper() {
        let dir = std::env::temp_dir().join(format!("ternvale-vmnet-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("vmnet.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        let port = Echo {
            rx: Vec::new(),
            info: PortInfo {
                mtu: 1500,
                max_packet: 1514,
                max_batch: 8,
                mac: [0x52, 0x54, 0, 0, 0, 1],
            },
        };
        std::thread::spawn(move || {
            let _ = serve(listener, port);
        });
        // SAFETY: this crate's tests run single-threaded around SOCK_ENV.
        unsafe {
            std::env::set_var(SOCK_ENV, &path);
        }
        let mut client = None;
        for _ in 0..50 {
            if let Ok(c) = Client::connect(&socket_path()) {
                client = Some(c);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let mut client = client.expect("connect");
        let frame = vec![0u8; 64];
        client.send_frame(&frame).expect("tx");
        let mut got = None;
        for _ in 0..50 {
            if let Some(f) = client.recv_frame().expect("rx") {
                got = Some(f);
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert_eq!(got, Some(frame));
        // SAFETY: paired with the set_var above.
        unsafe {
            std::env::remove_var(SOCK_ENV);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
