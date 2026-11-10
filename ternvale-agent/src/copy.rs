//! Guest-side file copy: rooted paths, SHA-256, resume from offset.

use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use sha2::{Digest, Sha256};
use ternvale_agent_proto::{
    encode_hex, is_guest_file_path, write_message, HexBytes, Message, MAX_BIN_CHUNK,
};

use crate::{AgentError, Stream};

/// Handle one copy request; write the reply on `stream`.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all, fields(kind = message.kind()))]
pub fn handle(stream: &mut Stream, message: &Message) -> Result<(), AgentError> {
    match message {
        Message::CopyPut {
            id,
            path,
            offset,
            data,
            last,
        } => put(stream, *id, path, *offset, &data.0, *last),
        Message::CopyGet {
            id,
            path,
            offset,
            len,
        } => get(stream, *id, path, *offset, *len),
        Message::CopyHash { id, path } => hash(stream, *id, path),
        _ => Ok(()),
    }
}

fn put(
    stream: &mut Stream,
    id: u64,
    path: &str,
    offset: u64,
    data: &[u8],
    last: bool,
) -> Result<(), AgentError> {
    if let Err(reason) = check_path(path) {
        return fail(stream, id, reason);
    }
    match write_at(Path::new(path), offset, data, last) {
        Ok(written) => {
            tracing::debug!(
                target: "ternvale::agent",
                id,
                path,
                offset,
                written,
                last,
                "copy_put"
            );
            reply(stream, &Message::CopyPutOk { id, written })
        }
        Err(error) => fail(stream, id, error.to_string()),
    }
}

fn get(stream: &mut Stream, id: u64, path: &str, offset: u64, len: u32) -> Result<(), AgentError> {
    if let Err(reason) = check_path(path) {
        return fail(stream, id, reason);
    }
    let n = (len as usize).min(MAX_BIN_CHUNK);
    match read_at(Path::new(path), offset, n) {
        Ok((buf, eof)) => reply(
            stream,
            &Message::CopyGetOk {
                id,
                data: HexBytes(buf),
                eof,
            },
        ),
        Err(error) => fail(stream, id, error.to_string()),
    }
}

fn hash(stream: &mut Stream, id: u64, path: &str) -> Result<(), AgentError> {
    if let Err(reason) = check_path(path) {
        return fail(stream, id, reason);
    }
    match sha256_file(Path::new(path)) {
        Ok((size, sha256)) => {
            tracing::debug!(target: "ternvale::agent", id, path, size, "copy_hash");
            reply(stream, &Message::CopyHashOk { id, size, sha256 })
        }
        Err(error) => fail(stream, id, error.to_string()),
    }
}

/// SHA-256 without holding the session lock, then send `CopyHashOk`.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(id, path)
)]
pub fn hash_reply(lock: &std::sync::Arc<std::sync::Mutex<Stream>>, id: u64, path: String) {
    let outcome = if let Err(reason) = check_path(&path) {
        Err(reason)
    } else {
        sha256_file(Path::new(&path)).map_err(|error| error.to_string())
    };
    let mut stream = match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let result = match outcome {
        Ok((size, sha256)) => {
            tracing::debug!(target: "ternvale::agent", id, path, size, "copy_hash");
            reply(&mut stream, &Message::CopyHashOk { id, size, sha256 })
        }
        Err(reason) => fail(&mut stream, id, reason),
    };
    if let Err(error) = result {
        tracing::warn!(target: "ternvale::agent", id, error = %error, "copy_hash reply failed");
    }
}

fn check_path(path: &str) -> Result<(), String> {
    if is_guest_file_path(path) {
        Ok(())
    } else {
        Err("copy path must be absolute without '..'".into())
    }
}

fn write_at(path: &Path, offset: u64, data: &[u8], last: bool) -> std::io::Result<u64> {
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)?;
        }
    }
    let mut file = if offset == 0 {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)?
    } else {
        OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(false)
            .open(path)?
    };
    let len = file.metadata()?.len();
    if offset > len {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            format!("copy offset {offset} past end {len}"),
        ));
    }
    file.seek(SeekFrom::Start(offset))?;
    file.write_all(data)?;
    if last {
        file.set_len(offset + data.len() as u64)?;
        file.sync_all()?;
    }
    Ok(data.len() as u64)
}

fn read_at(path: &Path, offset: u64, len: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut file = File::open(path)?;
    let size = file.metadata()?.len();
    if offset >= size {
        return Ok((Vec::new(), true));
    }
    file.seek(SeekFrom::Start(offset))?;
    let mut buf = vec![0u8; len];
    let n = file.read(&mut buf)?;
    buf.truncate(n);
    let eof = offset + n as u64 >= size;
    Ok((buf, eof))
}

fn sha256_file(path: &Path) -> std::io::Result<(u64, String)> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 8192];
    let mut size = 0u64;
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
        size += n as u64;
    }
    Ok((size, encode_hex(hasher.finalize().as_slice())))
}

fn fail(stream: &mut Stream, id: u64, reason: String) -> Result<(), AgentError> {
    tracing::warn!(target: "ternvale::agent", id, reason = %reason, "copy failed");
    let reason = if reason.len() > 256 {
        reason[..256].to_string()
    } else {
        reason
    };
    reply(stream, &Message::CopyErr { id, reason })
}

fn reply(stream: &mut Stream, message: &Message) -> Result<(), AgentError> {
    write_message(stream, message, "host").map_err(|source| AgentError::Frame {
        op: "send copy reply",
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn dir() -> PathBuf {
        let path = std::env::temp_dir().join(format!("tv-copy-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&path);
        path
    }

    #[test]
    fn write_read_hash_and_resume() {
        let path = dir().join("a.bin");
        write_at(&path, 0, b"hello", false).expect("put1");
        write_at(&path, 5, b" world", true).expect("put2");
        let (buf, eof) = read_at(&path, 0, 64).expect("get");
        assert!(eof);
        assert_eq!(buf, b"hello world");
        let (size, hash) = sha256_file(&path).expect("hash");
        assert_eq!(size, 11);
        assert_eq!(hash.len(), 64);
        assert!(check_path("/tmp/a").is_ok());
        assert!(check_path("/tmp/../etc/passwd").is_err());
    }
}
