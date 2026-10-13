//! What runs a TPM command once the CRB hands it over.
//!
//! [`TpmBackend`] takes one command byte stream and returns one response byte
//! stream. [`StubBackend`] implements no command semantics: it answers every
//! command with a 10-byte error response, so a guest driver sees a TPM that
//! talks but supports nothing.
//!
//! TPM 2.0 Library Specification, Part 1 §18 (command/response structure) and
//! Part 2 §6.9 (`TPM_ST`), §6.6 (`TPM_RC`): a command starts with `tag` (u16),
//! `commandSize` (u32, the whole command), `commandCode` (u32); a response
//! with `tag`, `responseSize`, `responseCode`. All big-endian. A response to
//! a command whose tag is not a TPM 2.0 tag uses `TPM_ST_RSP_COMMAND` and
//! `TPM_RC_BAD_TAG`, as TPM 1.2 software expects (Part 1 §18.4).
//! TODO(verify): the section numbers against the Library revision in use.

/// Bytes in a command or response header.
pub const HEADER_LEN: usize = 10;
/// `TPM_ST_NO_SESSIONS`.
pub const ST_NO_SESSIONS: u16 = 0x8001;
/// `TPM_ST_SESSIONS`.
pub const ST_SESSIONS: u16 = 0x8002;
/// `TPM_ST_RSP_COMMAND` (TPM 1.2 response tag).
pub const ST_RSP_COMMAND: u16 = 0x00c4;
/// `TPM_RC_FAILURE`: the TPM is in failure mode.
pub const RC_FAILURE: u32 = 0x101;
/// `TPM_RC_COMMAND_SIZE`: `commandSize` disagrees with the bytes sent.
pub const RC_COMMAND_SIZE: u32 = 0x142;
/// `TPM_RC_COMMAND_CODE`: command not implemented.
pub const RC_COMMAND_CODE: u32 = 0x143;
/// `TPM_RC_BAD_TAG`.
pub const RC_BAD_TAG: u32 = 0x01e;

/// Runs commands. Without [`TpmBackend::host_io`] it runs on the vCPU thread
/// that wrote `TPM_CRB_CTRL_START`, inside that write.
pub trait TpmBackend: Send {
    /// Short name for logs.
    fn name(&self) -> &str;

    /// Execute `command` (header included, `commandSize` bytes) issued at
    /// `locality` and return the whole response.
    fn execute(&mut self, locality: u8, command: &[u8]) -> Vec<u8>;

    /// True when [`TpmBackend::execute`] waits on host I/O. The CRB then
    /// runs it on a worker thread and keeps `START` at 1 until it returns,
    /// so the vCPU thread never blocks.
    fn host_io(&self) -> bool {
        false
    }
}

/// A parsed command header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandHeader {
    /// `tag`.
    pub tag: u16,
    /// `commandSize`.
    pub size: u32,
    /// `commandCode`.
    pub code: u32,
}

impl CommandHeader {
    /// The header at the start of `bytes`, if there are 10 bytes.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(len = bytes.len()))]
    pub fn parse(bytes: &[u8]) -> Option<Self> {
        let header = bytes.get(..HEADER_LEN)?;
        Some(Self {
            tag: u16::from_be_bytes([header[0], header[1]]),
            size: u32::from_be_bytes([header[2], header[3], header[4], header[5]]),
            code: u32::from_be_bytes([header[6], header[7], header[8], header[9]]),
        })
    }

    /// True for `TPM_ST_NO_SESSIONS` and `TPM_ST_SESSIONS`.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn is_tpm2(&self) -> bool {
        matches!(self.tag, ST_NO_SESSIONS | ST_SESSIONS)
    }
}

/// A header-only response: `tag`, `responseSize` 10, `code`.
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(tag = %format!("{tag:#06x}"), code = %format!("{code:#x}")))]
pub fn error_response(tag: u16, code: u32) -> Vec<u8> {
    let mut response = Vec::with_capacity(HEADER_LEN);
    response.extend_from_slice(&tag.to_be_bytes());
    response.extend_from_slice(&(HEADER_LEN as u32).to_be_bytes());
    response.extend_from_slice(&code.to_be_bytes());
    response
}

/// The response a TPM gives `command` before looking at its code: bad tag or
/// bad size, if either is wrong.
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(len = command.len()))]
pub fn header_error(command: &[u8]) -> Option<Vec<u8>> {
    let Some(header) = CommandHeader::parse(command) else {
        return Some(error_response(ST_NO_SESSIONS, RC_COMMAND_SIZE));
    };
    if !header.is_tpm2() {
        return Some(error_response(ST_RSP_COMMAND, RC_BAD_TAG));
    }
    if header.size as usize != command.len() {
        return Some(error_response(ST_NO_SESSIONS, RC_COMMAND_SIZE));
    }
    None
}

/// Answers every command with an error and implements nothing.
#[derive(Debug, Default)]
pub struct StubBackend {
    commands: u64,
}

impl StubBackend {
    /// A stub that has seen no commands.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn new() -> Self {
        Self::default()
    }

    /// Commands answered so far.
    #[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all)]
    pub fn commands(&self) -> u64 {
        self.commands
    }
}

impl TpmBackend for StubBackend {
    fn name(&self) -> &str {
        "stub"
    }

    fn execute(&mut self, locality: u8, command: &[u8]) -> Vec<u8> {
        self.commands += 1;
        let response = header_error(command)
            .unwrap_or_else(|| error_response(ST_NO_SESSIONS, RC_COMMAND_CODE));
        tracing::debug!(
            target: "ternvale::tpm",
            locality,
            code = %CommandHeader::parse(command).map_or("none".to_string(), |h| format!("{:#x}", h.code)),
            rc = %format!("{:#x}", u32::from_be_bytes([response[6], response[7], response[8], response[9]])),
            commands = self.commands,
            "tpm stub answered a command"
        );
        response
    }
}

/// `bytes` as lowercase hex pairs, for TRACE logs.
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(len = bytes.len()))]
pub fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(char::from(DIGITS[usize::from(byte >> 4)]));
        out.push(char::from(DIGITS[usize::from(byte & 0xf)]));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `TPM2_Startup(TPM_SU_CLEAR)`.
    const STARTUP: [u8; 12] = [0x80, 0x01, 0, 0, 0, 12, 0, 0, 0x01, 0x44, 0, 0];

    #[test]
    fn header_parses_big_endian_fields() {
        let header = CommandHeader::parse(&STARTUP).expect("header");
        assert_eq!(
            header,
            CommandHeader {
                tag: 0x8001,
                size: 12,
                code: 0x144
            }
        );
        assert!(header.is_tpm2());
        assert_eq!(CommandHeader::parse(&STARTUP[..9]), None);
    }

    #[test]
    fn stub_answers_command_code_not_implemented() {
        let mut stub = StubBackend::new();
        let response = stub.execute(0, &STARTUP);
        assert_eq!(response, [0x80, 0x01, 0, 0, 0, 10, 0, 0, 0x01, 0x43]);
        assert_eq!(stub.commands(), 1);
        assert_eq!(stub.name(), "stub");
    }

    #[test]
    fn stub_rejects_bad_tags_and_sizes_first() {
        let mut stub = StubBackend::new();
        let mut tpm12 = STARTUP;
        tpm12[..2].copy_from_slice(&0x00c1u16.to_be_bytes());
        assert_eq!(stub.execute(0, &tpm12), error_response(0x00c4, RC_BAD_TAG));
        assert_eq!(
            stub.execute(0, &STARTUP[..11]),
            error_response(ST_NO_SESSIONS, RC_COMMAND_SIZE)
        );
        assert_eq!(
            stub.execute(0, &[0x80]),
            error_response(ST_NO_SESSIONS, RC_COMMAND_SIZE)
        );
        assert_eq!(stub.commands(), 3);
    }

    #[test]
    fn hex_is_two_digits_per_byte() {
        assert_eq!(hex(&[0x80, 0x01, 0x0a]), "80010a");
        assert_eq!(hex(&[]), "");
    }
}
