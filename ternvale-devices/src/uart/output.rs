//! Guest TX bytes go to stdout and to the serial log file.

use std::io::{self, Write};
use std::path::Path;

/// Where transmitted bytes are appended.
pub trait ByteSink {
    /// Write guest TX bytes.
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()>;
}

/// Stdout plus an appended serial log file.
pub struct StdoutFile {
    file: std::fs::File,
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
        Ok(Self { file })
    }
}

impl ByteSink for StdoutFile {
    fn write_all(&mut self, bytes: &[u8]) -> io::Result<()> {
        let mut out = io::stdout().lock();
        out.write_all(bytes)?;
        out.flush()?;
        self.file.write_all(bytes)?;
        self.file.flush()?;
        Ok(())
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
