//! Classic libpcap writer (Ethernet link type) for Wireshark and tshark.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::{NetError, MAX_FRAME};

/// Environment variable naming the capture file.
pub const PCAP_ENV: &str = "TERNVALE_PCAP";

const MAGIC: u32 = 0xa1b2_c3d4;
const VERSION_MAJOR: u16 = 2;
const VERSION_MINOR: u16 = 4;
const LINKTYPE_ETHERNET: u32 = 1;

/// Appends frames to a pcap file. Each record is flushed so a capture can be read live.
pub struct PcapWriter {
    out: BufWriter<File>,
    path: PathBuf,
    packets: u64,
}

impl PcapWriter {
    /// Create (truncate) `path` and write the global header.
    #[tracing::instrument(level = "debug", target = "ternvale::net", fields(path = %path.display()))]
    pub fn create(path: &Path) -> Result<Self, NetError> {
        let fail = |source: std::io::Error| {
            tracing::error!(
                target: "ternvale::net",
                path = %path.display(),
                error = %source,
                "pcap create failed"
            );
            NetError::Pcap {
                path: path.to_path_buf(),
                source,
            }
        };
        let file = File::create(path).map_err(fail)?;
        let mut out = BufWriter::new(file);
        let mut header = Vec::with_capacity(24);
        header.extend_from_slice(&MAGIC.to_le_bytes());
        header.extend_from_slice(&VERSION_MAJOR.to_le_bytes());
        header.extend_from_slice(&VERSION_MINOR.to_le_bytes());
        header.extend_from_slice(&0i32.to_le_bytes());
        header.extend_from_slice(&0u32.to_le_bytes());
        header.extend_from_slice(&(MAX_FRAME as u32).to_le_bytes());
        header.extend_from_slice(&LINKTYPE_ETHERNET.to_le_bytes());
        out.write_all(&header).map_err(fail)?;
        out.flush().map_err(fail)?;
        tracing::info!(target: "ternvale::net", path = %path.display(), "pcap capture started");
        Ok(Self {
            out,
            path: path.to_path_buf(),
            packets: 0,
        })
    }

    /// Open the file named by [`PCAP_ENV`], or `None` when it is unset or empty.
    #[tracing::instrument(level = "debug", target = "ternvale::net")]
    pub fn from_env() -> Result<Option<Self>, NetError> {
        match std::env::var_os(PCAP_ENV) {
            Some(path) if !path.is_empty() => Self::create(Path::new(&path)).map(Some),
            _ => {
                tracing::debug!(target: "ternvale::net", "pcap capture off");
                Ok(None)
            }
        }
    }

    /// Append one frame stamped with the current wall-clock time.
    #[tracing::instrument(level = "trace", target = "ternvale::net", skip_all, fields(len = frame.len()))]
    pub fn write(&mut self, frame: &[u8]) -> Result<(), NetError> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let len = frame.len().min(MAX_FRAME) as u32;
        let mut record = Vec::with_capacity(16 + len as usize);
        record.extend_from_slice(&(now.as_secs() as u32).to_le_bytes());
        record.extend_from_slice(&now.subsec_micros().to_le_bytes());
        record.extend_from_slice(&len.to_le_bytes());
        record.extend_from_slice(&(frame.len() as u32).to_le_bytes());
        record.extend_from_slice(&frame[..len as usize]);
        let result = self.out.write_all(&record).and_then(|()| self.out.flush());
        result.map_err(|source| NetError::Pcap {
            path: self.path.clone(),
            source,
        })?;
        self.packets += 1;
        tracing::trace!(
            target: "ternvale::net",
            packets = self.packets,
            len,
            "pcap record written"
        );
        Ok(())
    }

    /// Records written so far.
    #[tracing::instrument(level = "debug", target = "ternvale::net", skip_all)]
    pub fn packets(&self) -> u64 {
        self.packets
    }
}

#[cfg(test)]
mod tests {
    use super::PcapWriter;

    #[test]
    fn writes_global_header_and_records() {
        let path = std::env::temp_dir().join(format!("ternvale-pcap-{}.pcap", std::process::id()));
        let mut writer = PcapWriter::create(&path).expect("create");
        writer.write(&[0xaa; 42]).expect("write");
        writer.write(&[0xbb; 60]).expect("write");
        assert_eq!(writer.packets(), 2);
        drop(writer);
        let bytes = std::fs::read(&path).expect("read");
        std::fs::remove_file(&path).expect("remove");
        assert_eq!(&bytes[0..4], &0xa1b2_c3d4u32.to_le_bytes());
        assert_eq!(u16::from_le_bytes([bytes[4], bytes[5]]), 2);
        assert_eq!(u16::from_le_bytes([bytes[6], bytes[7]]), 4);
        assert_eq!(&bytes[20..24], &1u32.to_le_bytes(), "linktype ethernet");
        let first_len = u32::from_le_bytes(bytes[32..36].try_into().expect("len"));
        assert_eq!(first_len, 42);
        assert_eq!(bytes.len(), 24 + 16 + 42 + 16 + 60);
        assert_eq!(bytes[24 + 16 + 42 + 16], 0xbb);
    }
}
