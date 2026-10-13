//! swtpm's control channel (`--ctrl type=unixio`).
//!
//! A request is a big-endian u32 command code (`CMD_*`) followed by that
//! command's request body; the reply is the response body alone, starting
//! with a big-endian u32 result (`ptm_res`, 0 = `TPM_SUCCESS`). Codes and
//! layouts: swtpm `include/swtpm/tpm_ioctl.h`, "Commands used by the non-CUSE
//! TPMs" ("All messages container big-endian data"); framing and reply
//! sizes: `src/swtpm/ctrlchannel.c` `ctrlchannel_process_fd`.

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use super::SwtpmError;
use crate::backend::hex;

/// `CMD_GET_CAPABILITY`: reply `ptm_res`, u32 capability bits.
pub(super) const CMD_GET_CAPABILITY: u32 = 0x01;
/// `CMD_INIT`: request u32 `init_flags`; reply `ptm_res`.
pub(super) const CMD_INIT: u32 = 0x02;
/// `CMD_SHUTDOWN`: reply `ptm_res`; swtpm then exits.
pub(super) const CMD_SHUTDOWN: u32 = 0x03;
/// `CMD_SET_LOCALITY`: request u8 locality; reply `ptm_res`.
pub(super) const CMD_SET_LOCALITY: u32 = 0x05;
/// `CMD_SET_BUFFERSIZE`: request u32 size; reply `ptm_res`, size in use,
/// minimum, maximum. Only before `CMD_INIT` (swtpm rejects it while the TPM
/// runs).
pub(super) const CMD_SET_BUFFERSIZE: u32 = 0x11;

/// `PTM_CAP_INIT`, `PTM_CAP_SHUTDOWN`, `PTM_CAP_SET_LOCALITY`,
/// `PTM_CAP_SET_BUFFERSIZE`: the commands Ternvale sends.
pub(super) const REQUIRED_CAPS: u32 = 1 | 1 << 1 | 1 << 3 | 1 << 13;

/// A control request waits at most this long for its reply.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(10);

/// `code` and `body` as one request.
pub(super) fn encode(code: u32, body: &[u8]) -> Vec<u8> {
    let mut request = Vec::with_capacity(4 + body.len());
    request.extend_from_slice(&code.to_be_bytes());
    request.extend_from_slice(body);
    request
}

/// Name for logs and errors.
pub(super) fn command_name(code: u32) -> &'static str {
    match code {
        CMD_GET_CAPABILITY => "CMD_GET_CAPABILITY",
        CMD_INIT => "CMD_INIT",
        CMD_SHUTDOWN => "CMD_SHUTDOWN",
        CMD_SET_LOCALITY => "CMD_SET_LOCALITY",
        CMD_SET_BUFFERSIZE => "CMD_SET_BUFFERSIZE",
        _ => "CMD_?",
    }
}

/// The client end of the control socket.
#[derive(Debug)]
pub(super) struct Control {
    stream: UnixStream,
}

impl Control {
    pub(super) fn new(stream: UnixStream) -> Result<Self, SwtpmError> {
        stream
            .set_read_timeout(Some(CONTROL_TIMEOUT))
            .and_then(|()| stream.set_write_timeout(Some(CONTROL_TIMEOUT)))
            .map_err(|error| SwtpmError::Control {
                command: "socket setup",
                reason: error.to_string(),
            })?;
        Ok(Self { stream })
    }

    /// Send `code` + `body`, read `reply_len` bytes, and require result 0.
    fn call(&mut self, code: u32, body: &[u8], reply_len: usize) -> Result<Vec<u8>, SwtpmError> {
        let command = command_name(code);
        let failed = |reason: String| {
            tracing::error!(target: "ternvale::tpm", command, reason = %reason, "swtpm control command failed");
            SwtpmError::Control { command, reason }
        };
        let request = encode(code, body);
        self.stream
            .write_all(&request)
            .map_err(|error| failed(format!("write: {error}")))?;
        let mut reply = vec![0; reply_len];
        self.stream
            .read_exact(&mut reply)
            .map_err(|error| failed(format!("read {reply_len}-byte reply: {error}")))?;
        let result = u32::from_be_bytes([reply[0], reply[1], reply[2], reply[3]]);
        tracing::trace!(target: "ternvale::tpm", command, request = %hex(&request), reply = %hex(&reply), result = %format!("{result:#x}"), "swtpm control");
        if result != 0 {
            return Err(failed(format!("result {result:#x}")));
        }
        Ok(reply)
    }

    /// `PTM_CAP_*` bits.
    pub(super) fn capabilities(&mut self) -> Result<u32, SwtpmError> {
        let reply = self.call(CMD_GET_CAPABILITY, &[], 8)?;
        Ok(u32::from_be_bytes([reply[4], reply[5], reply[6], reply[7]]))
    }

    /// Ask for a `size`-byte buffer; returns (in use, minimum, maximum).
    pub(super) fn set_buffer_size(&mut self, size: u32) -> Result<(u32, u32, u32), SwtpmError> {
        let reply = self.call(CMD_SET_BUFFERSIZE, &size.to_be_bytes(), 16)?;
        let word = |at: usize| {
            u32::from_be_bytes([reply[at], reply[at + 1], reply[at + 2], reply[at + 3]])
        };
        Ok((word(4), word(8), word(12)))
    }

    /// Start the TPM (`TPM_Init`); the guest then sends `TPM2_Startup`.
    pub(super) fn init(&mut self) -> Result<(), SwtpmError> {
        self.call(CMD_INIT, &0u32.to_be_bytes(), 4).map(drop)
    }

    /// Locality for the following data-channel commands.
    pub(super) fn set_locality(&mut self, locality: u8) -> Result<(), SwtpmError> {
        self.call(CMD_SET_LOCALITY, &[locality], 4).map(drop)
    }

    /// Stop the TPM (swtpm sends `TPM2_Shutdown` first if the guest did not)
    /// and make swtpm exit.
    pub(super) fn shutdown(&mut self) -> Result<(), SwtpmError> {
        self.call(CMD_SHUTDOWN, &[], 4).map(drop)
    }
}
