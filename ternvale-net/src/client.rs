//! Non-blocking Unix-socket client used by the VMM.

use std::collections::VecDeque;
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;

use super::protocol::{self, Hello, Msg};

/// Connected helper session.
pub struct Client {
    stream: UnixStream,
    hello: Hello,
    buf: Vec<u8>,
    rx: VecDeque<Vec<u8>>,
    up: bool,
}

impl Client {
    #[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all, fields(path = %path.display()))]
    pub fn connect(path: &Path) -> io::Result<Self> {
        let mut stream = UnixStream::connect(path).map_err(|error| {
            tracing::error!(
                target: "ternvale::net::vmnet",
                path = %path.display(),
                error = %error,
                "vmnet helper socket connect failed"
            );
            error
        })?;
        let hello = Hello::read_from(&mut stream)?;
        stream.set_nonblocking(true)?;
        tracing::info!(
            target: "ternvale::net::vmnet",
            path = %path.display(),
            mtu = hello.mtu,
            max_packet = hello.max_packet,
            max_batch = hello.max_batch,
            mac = %format!(
                "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
                hello.mac[0], hello.mac[1], hello.mac[2], hello.mac[3], hello.mac[4], hello.mac[5]
            ),
            "vmnet helper link up"
        );
        Ok(Self {
            stream,
            hello,
            buf: Vec::new(),
            rx: VecDeque::new(),
            up: true,
        })
    }

    /// Last LINK message, or false after the helper closed the socket.
    #[tracing::instrument(level = "trace", target = "ternvale::net::vmnet", skip_all)]
    pub fn link_up(&self) -> bool {
        self.up
    }

    #[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
    pub fn hello(&self) -> Hello {
        self.hello
    }

    #[tracing::instrument(level = "trace", target = "ternvale::net::vmnet", skip_all)]
    pub fn send_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        if frame.len() as u32 > self.hello.max_packet {
            tracing::warn!(
                target: "ternvale::net::vmnet",
                len = frame.len(),
                max_packet = self.hello.max_packet,
                reason = "frame exceeds vmnet max-packet-size",
                "packet drop"
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "frame exceeds vmnet max-packet-size",
            ));
        }
        let eth_limit = self.hello.mtu.saturating_add(14);
        if self.hello.mtu > 0 && frame.len() as u32 > eth_limit {
            tracing::warn!(
                target: "ternvale::net::vmnet",
                len = frame.len(),
                mtu = self.hello.mtu,
                reason = "frame exceeds vmnet mtu",
                "packet drop"
            );
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "frame exceeds vmnet mtu",
            ));
        }
        // One worker thread owns the socket; block only for this write.
        self.stream.set_nonblocking(false)?;
        let result = Msg::Data(frame.to_vec()).write_to(&mut self.stream);
        self.stream.set_nonblocking(true)?;
        result
    }

    #[tracing::instrument(level = "trace", target = "ternvale::net::vmnet", skip_all)]
    pub fn recv_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        self.pump()?;
        Ok(self.rx.pop_front())
    }

    fn pump(&mut self) -> io::Result<()> {
        let mut tmp = [0u8; 8192];
        loop {
            match self.stream.read(&mut tmp) {
                Ok(0) => {
                    self.up = false;
                    tracing::warn!(
                        target: "ternvale::net::vmnet",
                        reason = "helper closed the socket",
                        "packet drop"
                    );
                    tracing::info!(
                        target: "ternvale::net::vmnet",
                        up = false,
                        "vmnet link state"
                    );
                    return Err(io::Error::new(
                        io::ErrorKind::ConnectionAborted,
                        "vmnet helper closed",
                    ));
                }
                Ok(n) => self.buf.extend_from_slice(&tmp[..n]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error),
            }
            if self.buf.len() > 1024 * 1024 {
                tracing::warn!(
                    target: "ternvale::net::vmnet",
                    reason = "helper rx buffer unbounded; resetting",
                    "packet drop"
                );
                self.buf.clear();
                return Err(io::Error::other("vmnet helper rx overflow"));
            }
        }
        while let Some(msg) = protocol::take_msg(&mut self.buf) {
            match msg? {
                Msg::Data(frame) => self.rx.push_back(frame),
                Msg::Link(up) => {
                    self.up = up;
                    tracing::info!(
                        target: "ternvale::net::vmnet",
                        up,
                        "vmnet link state"
                    );
                }
                Msg::Drop(reason) => tracing::warn!(
                    target: "ternvale::net::vmnet",
                    reason = %reason,
                    "packet drop"
                ),
            }
        }
        Ok(())
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.stream.flush();
        tracing::info!(target: "ternvale::net::vmnet", "vmnet helper link down");
    }
}

#[cfg(test)]
mod tests {
    use super::Client;
    use crate::protocol::{Hello, Msg};
    use std::os::unix::net::UnixListener;
    use std::thread;
    use std::time::Duration;

    #[test]
    fn link_down_after_helper_eof() {
        let dir = std::env::temp_dir().join(format!("ternvale-vmnet-eof-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("vmnet.sock");
        let listener = UnixListener::bind(&path).expect("bind");
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept");
            Hello {
                mtu: 1500,
                max_packet: 1514,
                max_batch: 1,
                mac: [0; 6],
            }
            .write_to(&mut stream)
            .expect("hello");
            Msg::Link(true).write_to(&mut stream).expect("link");
            let _ = stream.shutdown(std::net::Shutdown::Both);
        });
        let mut client = None;
        for _ in 0..50 {
            if let Ok(c) = Client::connect(&path) {
                client = Some(c);
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        let mut client = client.expect("connect");
        let mut down = false;
        for _ in 0..50 {
            match client.recv_frame() {
                Ok(_) => thread::sleep(Duration::from_millis(20)),
                Err(_) => {
                    down = true;
                    break;
                }
            }
        }
        assert!(down, "eof should fail recv");
        assert!(!client.link_up());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
