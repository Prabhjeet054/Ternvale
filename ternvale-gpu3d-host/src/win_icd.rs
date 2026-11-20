//! Prototype Windows Vulkan ICD command-stream encoder (host-testable).
//!
//! Independent of Mesa Venus and Yttrium. The Windows ICD (`drivers/tvvk`)
//! emits the same length-prefixed CS as `decode_submit`. Opcode 1 is a
//! **nop** with an 8-byte header and no object ids.
//!
//! TODO(verify): opcode 1 is **not** a Mesa Venus command; it exists so the
//! host decoder and guest encoder can round-trip before a real protocol.

use crate::decoder::{decode_submit, CsCommand};

/// Prototype nop opcode used by `drivers/tvvk` (`encode.c`).
pub const TVVK_CS_NOP: u32 = 1;
/// Header size (`cmd_type` + `cmd_size`) in bytes.
pub const TVVK_CS_HEADER: u32 = 8;

/// Encode a nop SUBMIT_3D payload (8 bytes, no object ids).
#[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
pub fn encode_nop() -> Vec<u8> {
    let mut out = Vec::with_capacity(TVVK_CS_HEADER as usize);
    out.extend_from_slice(&TVVK_CS_NOP.to_le_bytes());
    out.extend_from_slice(&TVVK_CS_HEADER.to_le_bytes());
    tracing::debug!(
        target: "ternvale::gpu3d",
        opcode = TVVK_CS_NOP,
        size = TVVK_CS_HEADER,
        "encoded tvvk prototype nop CS"
    );
    out
}

/// Decode `encode_nop()` and require a single opcode-1 command with no ids.
#[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
pub fn decode_nop(bytes: &[u8]) -> Result<CsCommand, crate::Gpu3dError> {
    let cmds = decode_submit(bytes)?;
    match cmds.as_slice() {
        [cmd] if cmd.opcode == TVVK_CS_NOP && cmd.object_ids.is_empty() => Ok(cmd.clone()),
        _ => {
            tracing::warn!(
                target: "ternvale::gpu3d",
                n = cmds.len(),
                "tvvk nop decode mismatch"
            );
            Err(crate::Gpu3dError::Decode("expected single nop CS"))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::decoder::check_object_ids;
    use std::collections::HashSet;

    #[test]
    fn encode_nop_round_trips_decoder() {
        let bytes = encode_nop();
        assert_eq!(bytes.len(), 8);
        assert_eq!(&bytes[0..4], &1u32.to_le_bytes());
        assert_eq!(&bytes[4..8], &8u32.to_le_bytes());
        let cmd = decode_nop(&bytes).expect("nop");
        assert_eq!(cmd.opcode, 1);
        assert_eq!(cmd.size, 8);
        check_object_ids(&[cmd], &HashSet::new()).expect("no ids");
    }

    #[test]
    fn decode_nop_rejects_object_ids() {
        let mut bytes = encode_nop();
        bytes[4..8].copy_from_slice(&12u32.to_le_bytes());
        bytes.extend_from_slice(&7u32.to_le_bytes());
        assert!(decode_nop(&bytes).is_err());
    }

    #[test]
    fn guest_icd_json_and_encode_c_match_nop() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..");
        let json = std::fs::read_to_string(root.join("drivers/tvvk/tvvk.json")).expect("json");
        assert!(json.contains("tvvk.dll"), "{json}");
        assert!(json.contains("1.0.0"), "{json}");
        let encode = std::fs::read_to_string(root.join("drivers/tvvk/encode.h")).expect("h");
        assert!(encode.contains("TVVK_CS_NOP 1"), "{encode}");
        let inf = std::fs::read_to_string(root.join("drivers/tvvk/tvvk.inf")).expect("inf");
        assert!(inf.contains("Khronos\\Vulkan\\Drivers"), "{inf}");
    }
}
