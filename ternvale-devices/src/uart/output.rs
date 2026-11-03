//! Guest TX bytes go to stdout and to the serial log file.

use std::io::{self, Write};
use std::path::Path;

/// Where transmitted bytes are appended. `Send` because the UART is shared by
/// every vCPU thread.
pub trait ByteSink: Send {
    /// Write guest TX bytes.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
}

const TX_BATCH: usize = 256;

/// Stdout plus an appended serial log file.
pub struct StdoutFile {
    file: std::fs::File,
    buf: Vec<u8>,
}

impl StdoutFile {
    /// Create or append `path`. Parent directories are created.
    #[tracing::instrument(level = "debug", target = "ternvale::uart", skip_all, fields(path = %path.display()))]
    pub fn create(path: &Path) -> Result<Self, UartError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                std::fs::create_dir_all(parent).map_err(|source| UartError::Create {
                    path: path.to_path_buf(),
                    source,
                })?;
            }
        }
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .map_err(|source| UartError::Create {
                path: path.to_path_buf(),
                source,
            })?;
        tracing::info!(
            target: "ternvale::uart",
            path = %path.display(),
            "serial log opened"
        );
        Ok(Self {
            file,
            buf: Vec::with_capacity(TX_BATCH),
        })
    }

    fn flush_buf(&mut self) -> io::Result<()> {
        if self.buf.is_empty() {
            return Ok(());
        }
        let mut out = io::stdout().lock();
        out.write_all(&self.buf)?;
        out.flush()?;
        self.file.write_all(&self.buf)?;
        self.file.flush()?;
        self.buf.clear();
        Ok(())
    }
}

impl ByteSink for StdoutFile {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.buf.extend_from_slice(bytes);
        if self.buf.len() >= TX_BATCH || bytes.contains(&b'\n') {
            self.flush_buf()?;
        }
        Ok(())
    }
}

impl Drop for StdoutFile {
    fn drop(&mut self) {
        if let Err(error) = self.flush_buf() {
            tracing::error!(
                target: "ternvale::uart",
                error = %error,
                "uart tx batch flush failed"
            );
        }
    }
}

impl ByteSink for Vec<u8> {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.extend_from_slice(bytes);
        Ok(())
    }
}

/// Opening the serial log failed.
#[derive(Debug, thiserror::Error)]
pub enum UartError {
    /// The serial log file could not be created.
    #[error("create serial log {}: {source}", path.display())]
    Create {
        /// Requested path.
        path: std::path::PathBuf,
        /// Filesystem error.
        source: io::Error,
    },
}

/// Holds TX bytes until a newline, then returns the line without that newline.
#[derive(Debug, Default)]
pub(super) struct LineBuf {
    buf: String,
}

impl LineBuf {
    pub(super) fn push(&mut self, byte: u8) -> Option<String> {
        if byte == b'\n' {
            return self.flush();
        }
        self.buf.push(char::from(byte));
        if self.buf.len() >= 256 {
            return self.flush();
        }
        None
    }

    pub(super) fn flush(&mut self) -> Option<String> {
        if self.buf.is_empty() {
            None
        } else {
            Some(std::mem::take(&mut self.buf))
        }
    }
}
