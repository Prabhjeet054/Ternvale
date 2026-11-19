//! Untrusted Venus / virtio-gpu `SUBMIT_3D` command-stream decoder.
//!
//! Guest bytes are hostile (Part 3 security rules): no panic, no unbounded
//! allocation, no infinite loop. Layout is a length-prefixed dword stream
//! (`cmd_type:u32`, `cmd_size:u32` including the 8-byte header, then body).
//! That matches virglrenderer Venus CS framing; opcode numbers are treated
//! as opaque u32s and are not copied from Mesa headers.
//!
//! TODO(verify): opcode ranges against the virglrenderer / venus-protocol
//! revision paired with Mesa Venus in the guest.

use std::collections::HashSet;

use crate::Gpu3dError;

/// Largest `SUBMIT_3D` payload this decoder will walk.
pub const MAX_SUBMIT: usize = 4 * 1024 * 1024;
/// Hard cap on CS commands in one submit.
pub const MAX_COMMANDS: usize = 4096;
/// Hard cap on object ids recorded from one submit (and globally).
pub const MAX_IDS_PER_SUBMIT: usize = 1024;

/// One validated CS command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CsCommand {
    /// Opaque Venus command type.
    pub opcode: u32,
    /// Size including the 8-byte header.
    pub size: u32,
    /// Object ids pulled from the body (every aligned u32 that is non-zero).
    pub object_ids: Vec<u32>,
}

/// Decode a SUBMIT_3D payload. Empty payload is valid (no-op).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::gpu3d",
    skip_all,
    fields(len = bytes.len())
)]
pub fn decode_submit(bytes: &[u8]) -> Result<Vec<CsCommand>, Gpu3dError> {
    if bytes.len() > MAX_SUBMIT {
        tracing::warn!(
            target: "ternvale::gpu3d",
            len = bytes.len(),
            max = MAX_SUBMIT,
            "submit_3d payload above cap"
        );
        return Err(Gpu3dError::Limit("submit payload too large"));
    }
    if bytes.len() % 4 != 0 {
        tracing::warn!(
            target: "ternvale::gpu3d",
            len = bytes.len(),
            "submit_3d length is not a multiple of 4"
        );
        return Err(Gpu3dError::Decode("payload is not dword-aligned"));
    }
    let mut out = Vec::new();
    let mut off = 0usize;
    while off < bytes.len() {
        if out.len() >= MAX_COMMANDS {
            return Err(Gpu3dError::Limit("too many CS commands"));
        }
        if bytes.len() - off < 8 {
            return Err(Gpu3dError::Decode("truncated CS header"));
        }
        let opcode = read_u32(bytes, off);
        let size = read_u32(bytes, off + 4);
        if size < 8 || size % 4 != 0 {
            return Err(Gpu3dError::Decode("invalid CS command size"));
        }
        let span = size as usize;
        if off.checked_add(span).is_none_or(|end| end > bytes.len()) {
            return Err(Gpu3dError::Decode("CS command overruns buffer"));
        }
        let body = &bytes[off + 8..off + span];
        let object_ids = collect_ids(body)?;
        tracing::trace!(
            target: "ternvale::gpu3d",
            opcode,
            size,
            ids = object_ids.len(),
            "decoded venus CS command"
        );
        out.push(CsCommand {
            opcode,
            size,
            object_ids,
        });
        off += span;
    }
    Ok(out)
}

/// Reject object ids that are not in `known` (0 is never valid).
#[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
pub fn check_object_ids(cmds: &[CsCommand], known: &HashSet<u32>) -> Result<(), Gpu3dError> {
    for cmd in cmds {
        for &id in &cmd.object_ids {
            if id == 0 {
                return Err(Gpu3dError::BadId {
                    id: 0,
                    kind: "object",
                });
            }
            if !known.contains(&id) {
                tracing::warn!(
                    target: "ternvale::gpu3d",
                    id,
                    opcode = cmd.opcode,
                    "unknown object id in submit"
                );
                return Err(Gpu3dError::BadId {
                    id: u64::from(id),
                    kind: "object",
                });
            }
        }
    }
    Ok(())
}

fn read_u32(bytes: &[u8], off: usize) -> u32 {
    let mut raw = [0u8; 4];
    if off + 4 <= bytes.len() {
        raw.copy_from_slice(&bytes[off..off + 4]);
    }
    u32::from_le_bytes(raw)
}

fn collect_ids(body: &[u8]) -> Result<Vec<u32>, Gpu3dError> {
    let mut ids = Vec::new();
    let mut i = 0;
    while i + 4 <= body.len() {
        let word = read_u32(body, i);
        if word != 0 {
            if ids.len() >= MAX_IDS_PER_SUBMIT {
                return Err(Gpu3dError::Limit("too many object ids"));
            }
            ids.push(word);
        }
        i += 4;
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_submit_is_ok() {
        assert!(decode_submit(&[]).expect("empty").is_empty());
    }

    #[test]
    fn rejects_odd_length() {
        assert!(matches!(
            decode_submit(&[1, 2, 3]),
            Err(Gpu3dError::Decode(_))
        ));
    }

    #[test]
    fn parses_one_command_and_ids() {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&1u32.to_le_bytes());
        bytes.extend_from_slice(&16u32.to_le_bytes());
        bytes.extend_from_slice(&7u32.to_le_bytes());
        bytes.extend_from_slice(&0u32.to_le_bytes());
        let cmds = decode_submit(&bytes).expect("cmd");
        assert_eq!(cmds.len(), 1);
        assert_eq!(cmds[0].opcode, 1);
        assert_eq!(cmds[0].object_ids, vec![7]);
    }

    #[test]
    fn unknown_id_is_rejected() {
        let cmd = CsCommand {
            opcode: 1,
            size: 12,
            object_ids: vec![9],
        };
        let known = HashSet::new();
        assert!(check_object_ids(&[cmd], &known).is_err());
    }
}
