//! Local HTTP Range resume + checksum for [`super::download`].

use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use super::download;

fn read_headers(stream: &mut TcpStream) -> std::io::Result<String> {
    stream.set_read_timeout(Some(Duration::from_secs(8)))?;
    let mut acc = Vec::new();
    let mut buf = [0u8; 256];
    loop {
        let n = stream.read(&mut buf)?;
        if n == 0 {
            break;
        }
        acc.extend_from_slice(&buf[..n]);
        if acc.windows(4).any(|w| w == b"\r\n\r\n") || acc.len() > 8 * 1024 {
            break;
        }
    }
    Ok(String::from_utf8_lossy(&acc).into_owned())
}

fn range_start(headers: &str) -> Option<usize> {
    let line = headers
        .lines()
        .find(|l| l.to_ascii_lowercase().starts_with("range:"))?;
    let spec = line.split_once(':')?.1.trim();
    let spec = spec.strip_prefix("bytes=")?;
    let start = spec.split('-').next()?;
    start.parse().ok()
}

fn serve(payload: Arc<Vec<u8>>, drop_first: Arc<AtomicBool>) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let addr = listener.local_addr().expect("addr");
    thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(mut stream) = incoming else { continue };
            let Ok(headers) = read_headers(&mut stream) else {
                continue;
            };
            let body: Vec<u8>;
            let head: String;
            if let Some(start) = range_start(&headers) {
                let start = start.min(payload.len());
                body = payload[start..].to_vec();
                let end = payload.len().saturating_sub(1);
                head = format!(
                    "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes {start}-{end}/{total}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
                    total = payload.len(),
                    len = body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            } else if drop_first.swap(false, Ordering::SeqCst) {
                let half = payload.len() / 2;
                head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {total}\r\nConnection: close\r\n\r\n",
                    total = payload.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&payload[..half]);
            } else {
                body = payload.as_ref().clone();
                head = format!(
                    "HTTP/1.1 200 OK\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n",
                    len = body.len()
                );
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
            let _ = stream.flush();
            let _ = stream.shutdown(Shutdown::Both);
        }
    });
    format!("http://{addr}/blob.bin")
}

fn temp_dest() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tv-ipsw-resume-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::create_dir_all(&dir).expect("dir");
    dir.join("restore.ipsw")
}

#[test]
fn interrupted_download_resumes_and_checksum_matches() {
    let mut payload = vec![0u8; 2 * 1024 * 1024];
    for (i, b) in payload.iter_mut().enumerate() {
        *b = (i % 251) as u8;
    }
    let payload = Arc::new(payload);
    let drop_first = Arc::new(AtomicBool::new(true));
    let url = serve(Arc::clone(&payload), Arc::clone(&drop_first));
    let dest = temp_dest();
    let part = PathBuf::from(format!("{}.part", dest.display()));

    let first = download(&url, &dest, None, |_, _, _| {});
    assert!(
        first.is_err(),
        "first transfer must fail after the drop: {first:?}"
    );
    assert!(part.is_file(), "partial .part must remain for resume");
    let have = std::fs::metadata(&part).expect("part meta").len();
    assert!(have > 0 && have < payload.len() as u64, "part={have}");

    let last = Arc::new(AtomicU32::new(0));
    let last2 = Arc::clone(&last);
    let went_back = Arc::new(AtomicBool::new(false));
    let back = Arc::clone(&went_back);
    let result = download(&url, &dest, None, move |_, _, permille| {
        let prev = last2.load(Ordering::Relaxed);
        if permille < prev {
            back.store(true, Ordering::Relaxed);
        }
        last2.store(permille, Ordering::Relaxed);
    })
    .expect("resume");
    assert!(
        !went_back.load(Ordering::Relaxed),
        "progress went backwards"
    );
    assert!(result.resumed, "resume flag");
    assert_eq!(result.bytes, payload.len() as u64);
    let got = std::fs::read(&dest).expect("dest");
    assert_eq!(got.as_slice(), payload.as_slice());
    assert!(!part.exists(), "part consumed");
    assert!(!result.sha256.is_empty());

    let again = download(&url, &dest, Some(&result.sha256), |_, _, _| {}).expect("cached");
    assert_eq!(again.sha256, result.sha256);
    assert!(!again.resumed);
    let _ = std::fs::remove_dir_all(dest.parent().expect("parent"));
}
