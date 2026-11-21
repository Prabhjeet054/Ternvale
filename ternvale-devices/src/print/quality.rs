//! Step 115 host analogs: PDF→Preview, CUPS jobs, tunnel refusals, opt-in off.

use std::io::{Read, Write};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use tracing::field::{Field, Visit};
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Metadata, Subscriber};

use crate::print::cups::forward_to_cups;
use crate::print::http::Allowed;
use crate::print::ops::{OP_CUPS_DELETE_PRINTER, OP_PRINT_JOB};
use crate::print::pdf::OpenPdf;
use crate::print::policy::noop_open;
use crate::print::proxy::dispatch;
use crate::print::PrintPolicy;
use ternvale_config::PrintSection;

/// Minimal one-page PDF a Windows “Print test page” analog would send.
const TEST_PAGE: &[u8] = b"%PDF-1.4\n1 0 obj<</Type/Catalog>>endobj\ntrailer<<>>\n%%EOF\n";

struct LogBuf(Arc<Mutex<String>>);

struct FieldSink(String);

impl Visit for FieldSink {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        self.0.push_str(&format!(" {}={value:?}", field.name()));
    }
    fn record_str(&mut self, field: &Field, value: &str) {
        self.0.push_str(&format!(" {}={value}", field.name()));
    }
    fn record_u64(&mut self, field: &Field, value: u64) {
        self.0.push_str(&format!(" {}={value}", field.name()));
    }
}

impl Subscriber for LogBuf {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.target().starts_with("ternvale::print")
    }
    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        Id::from_u64(1)
    }
    fn record(&self, _span: &Id, _values: &Record<'_>) {}
    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let mut sink = FieldSink(String::new());
        event.record(&mut sink);
        let mut g = self.0.lock().expect("print quality log");
        g.push_str(event.metadata().level().as_str());
        g.push(' ');
        g.push_str(event.metadata().target());
        g.push_str(&sink.0);
        g.push('\n');
    }

    fn enter(&self, _span: &Id) {}
    fn exit(&self, _span: &Id) {}
}

fn capture_logs(f: impl FnOnce()) -> String {
    let buf = Arc::new(Mutex::new(String::new()));
    let sub = LogBuf(Arc::clone(&buf));
    tracing::subscriber::with_default(sub, f);
    let logs = buf.lock().expect("print quality log").clone();
    logs
}

fn http_ipp(path: &str, host: &str, op: u16, extra: &[u8]) -> Vec<u8> {
    let mut body = vec![1, 1, (op >> 8) as u8, op as u8, 0, 0, 0, 1, 0x03];
    body.extend_from_slice(extra);
    let mut out = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    out.extend_from_slice(&body);
    out
}

fn status_line(resp: &[u8]) -> String {
    let text = String::from_utf8_lossy(resp);
    text.lines().next().unwrap_or("").to_string()
}

fn tmp_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tv-print-q-{}-{}-{tag}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("pdf dir");
    dir
}

struct MockCups {
    addr: SocketAddr,
    seen: Arc<Mutex<Vec<u8>>>,
    thread: Option<JoinHandle<()>>,
}

impl MockCups {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock cups");
        let addr = listener.local_addr().expect("local");
        let seen = Arc::new(Mutex::new(Vec::new()));
        let flag = Arc::clone(&seen);
        let thread = thread::spawn(move || {
            listener.set_nonblocking(false).expect("blocking mock cups");
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
                let mut buf = Vec::new();
                let mut tmp = [0u8; 4096];
                loop {
                    match stream.read(&mut tmp) {
                        Ok(0) => break,
                        Ok(n) => {
                            buf.extend_from_slice(&tmp[..n]);
                            if let Some(split) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                                if buf.len() > split + 4 {
                                    break;
                                }
                            }
                        }
                        Err(_) => break,
                    }
                }
                *flag.lock().expect("seen") = buf;
                let ipp = [1u8, 1, 0, 0, 0, 0, 0, 1, 0x03];
                let head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    ipp.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&ipp);
            }
        });
        Self {
            addr,
            seen,
            thread: Some(thread),
        }
    }

    fn join(mut self) -> Vec<u8> {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        self.seen.lock().expect("seen").clone()
    }
}

fn shared_policy(pdf_dir: PathBuf, open: OpenPdf, cups_addr: SocketAddr) -> PrintPolicy {
    let mut policy = PrintPolicy::from_config(
        "lab",
        &PrintSection {
            enabled: true,
            pdf: true,
            share: vec!["Office".into()],
        },
        &["Office".into()],
        pdf_dir,
        open,
    );
    policy.cups_addr = cups_addr;
    policy
}

fn preview_spy() -> (OpenPdf, Arc<Mutex<Option<PathBuf>>>) {
    let seen = Arc::new(Mutex::new(None));
    let flag = Arc::clone(&seen);
    let open: OpenPdf = Arc::new(move |path: &Path| {
        tracing::info!(
            target: "ternvale::print",
            path = %path.display(),
            "Preview analog opened print job"
        );
        *flag.lock().expect("preview spy") = Some(path.to_path_buf());
        Ok(())
    });
    (open, seen)
}

#[test]
fn windows_print_to_pdf_opens_preview_analog() {
    let dir = tmp_dir("pdf");
    let (open, seen) = preview_spy();
    let policy = PrintPolicy::from_config(
        "lab",
        &PrintSection {
            enabled: true,
            pdf: true,
            share: Vec::new(),
        },
        &[],
        dir,
        open,
    );
    let raw = http_ipp("/printers/pdf", "127.0.0.1:8631", OP_PRINT_JOB, TEST_PAGE);
    let resp = dispatch(&raw, &policy);
    assert!(
        status_line(&resp).contains("200"),
        "windows pdf job: {resp:?}"
    );
    let path = seen
        .lock()
        .expect("spy")
        .clone()
        .expect("Preview opened PDF");
    assert!(std::fs::read(&path).expect("pdf").starts_with(b"%PDF-1.4"));
    let _ = std::fs::remove_file(&path);
}

#[test]
fn windows_and_linux_print_to_cups_pdf_analog() {
    for (guest, host) in [("windows", "127.0.0.1:8631"), ("linux", "localhost:8631")] {
        let mock = MockCups::spawn();
        let addr = mock.addr;
        let dir = tmp_dir(guest);
        let policy = shared_policy(dir, noop_open(), addr);
        let raw = http_ipp(
            "/printers/Office",
            host,
            OP_PRINT_JOB,
            b"%!PS-Adobe-3.0\n%%EOF\n",
        );
        let resp = dispatch(&raw, &policy);
        let seen = mock.join();
        assert!(
            status_line(&resp).contains("200"),
            "{guest} cups job: {}",
            status_line(&resp)
        );
        let text = String::from_utf8_lossy(&seen);
        assert!(
            text.contains("POST /printers/Office"),
            "{guest} mock CUPS missed the job: {text}"
        );
        assert!(
            text.contains(&format!("Host: localhost:{}", addr.port())),
            "{guest} must rewrite Host to loopback CUPS, not guest Host {host}: {text}"
        );
        assert!(
            !text.contains("8631"),
            "{guest} must not connect to the guest IPP port: {text}"
        );
    }
}

#[test]
fn tunnel_refuses_admin_delete_and_arbitrary_ports_and_logs() {
    let dir = tmp_dir("deny");
    let policy = shared_policy(
        dir,
        noop_open(),
        SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 631),
    );
    let logs = capture_logs(|| {
        let admin_get = dispatch(
            b"GET /admin HTTP/1.1\r\nHost: localhost:631\r\n\r\n",
            &policy,
        );
        assert!(
            status_line(&admin_get).contains("403"),
            "GET /admin: {}",
            status_line(&admin_get)
        );
        let admin_post = dispatch(
            &http_ipp("/admin", "localhost:631", OP_PRINT_JOB, b""),
            &policy,
        );
        assert!(
            status_line(&admin_post).contains("403"),
            "POST /admin: {}",
            status_line(&admin_post)
        );
        let delete = dispatch(
            &http_ipp(
                "/printers/Office",
                "127.0.0.1:8631",
                OP_CUPS_DELETE_PRINTER,
                b"",
            ),
            &policy,
        );
        assert!(
            status_line(&delete).contains("403"),
            "CUPS-Delete-Printer: {}",
            status_line(&delete)
        );
        let connect = dispatch(b"CONNECT 127.0.0.1:22 HTTP/1.1\r\n\r\n", &policy);
        assert!(
            status_line(&connect).contains("405"),
            "CONNECT: {}",
            status_line(&connect)
        );
        let abs = dispatch(
            &http_ipp(
                "http://127.0.0.1:9/printers/pdf",
                "127.0.0.1:9",
                OP_PRINT_JOB,
                TEST_PAGE,
            ),
            &policy,
        );
        assert!(
            status_line(&abs).contains("403"),
            "absolute URL: {}",
            status_line(&abs)
        );
        let remote = SocketAddr::new(IpAddr::V4(Ipv4Addr::new(8, 8, 8, 8)), 631);
        let dummy = Allowed {
            printer_id: "Office".into(),
            op: OP_PRINT_JOB,
            request_id: 1,
            version_major: 1,
            version_minor: 1,
            body: vec![1, 1, 0, 2, 0, 0, 0, 1, 0x03],
        };
        let err = forward_to_cups(remote, "Office", &dummy).expect_err("non-loopback");
        assert!(err.contains("loopback"), "{err}");
    });
    assert!(
        logs.contains("blocked CUPS admin URL"),
        "admin URL must be logged: {logs}"
    );
    assert!(
        logs.contains("blocked IPP operation"),
        "printer deletion must be logged: {logs}"
    );
    assert!(
        logs.contains("blocked non-POST") || logs.contains("CONNECT"),
        "CONNECT / arbitrary method must be logged: {logs}"
    );
    assert!(
        logs.contains("refusing non-loopback CUPS address"),
        "non-loopback CUPS must be logged: {logs}"
    );

    // Guest Host: 10.0.0.1:9999 must not steer the forwarder off loopback CUPS.
    let mock = MockCups::spawn();
    let addr = mock.addr;
    let dir = tmp_dir("steer");
    let policy = shared_policy(dir, noop_open(), addr);
    let raw = http_ipp("/printers/Office", "10.0.0.1:9999", OP_PRINT_JOB, b"job");
    let resp = dispatch(&raw, &policy);
    let seen = mock.join();
    assert!(status_line(&resp).contains("200"));
    let text = String::from_utf8_lossy(&seen);
    assert!(text.contains("POST /printers/Office"));
    assert!(
        !text.contains("10.0.0.1") && !text.contains("9999"),
        "guest Host must not reach the mock: {text}"
    );
}

#[test]
fn print_opt_in_off_clears_guest_printers() {
    let dir = tmp_dir("off");
    let policy = PrintPolicy::from_config(
        "lab",
        &PrintSection {
            enabled: false,
            pdf: true,
            share: vec!["Office".into()],
        },
        &["Office".into()],
        dir,
        noop_open(),
    );
    assert!(
        policy.guest_printers().is_empty(),
        "opt-in off must hide Print to PDF and CUPS queues from the guest"
    );
    assert!(policy.resolve("pdf").is_none());
    assert!(policy.resolve("Office").is_none());
    let resp = dispatch(
        &http_ipp("/printers/pdf", "127.0.0.1:8631", OP_PRINT_JOB, TEST_PAGE),
        &policy,
    );
    assert!(
        status_line(&resp).contains("404"),
        "disabled printers must 404: {}",
        status_line(&resp)
    );
}
