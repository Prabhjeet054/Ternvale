//! Which TPM command and response bytes may leave the VM in logs and crash
//! reports.
//!
//! TPM traffic carries secrets: authorization values and HMACs in session
//! areas, sensitive parts of created and imported objects, unsealed data,
//! decrypted plaintext, NV contents, and random bytes the guest may turn
//! into keys. Bytes are shown only for the commands in [`PUBLIC_COMMANDS`],
//! and only when the command carries no sessions (`TPM_ST_NO_SESSIONS`).
//! Everything else is reduced to its command code, sizes and response code.
//!
//! Command codes: TPM 2.0 Library Part 2 (Structures), §6.5.2, Table 12
//! "Definition of (UINT32) TPM_CC Constants". TODO(verify): the names below
//! were taken from that table as mirrored by tpm2-tss `tss2_tpm2_types.h`;
//! re-check them against the spec revision Ternvale targets. A wrong name
//! only mislabels a report line; [`PUBLIC_COMMANDS`] decides what is shown.

use crate::backend::{CommandHeader, ST_NO_SESSIONS};

/// Commands whose parameters and responses hold no secrets: start-up and
/// self test, capability and clock queries, PCR values, public areas of
/// objects and NV indices, and handle flushes.
pub const PUBLIC_COMMANDS: [u32; 12] = [
    0x142, // IncrementalSelfTest
    0x143, // SelfTest
    0x144, // Startup
    0x145, // Shutdown
    0x165, // FlushContext
    0x169, // NV_ReadPublic
    0x173, // ReadPublic
    0x17A, // GetCapability
    0x17C, // GetTestResult
    0x17E, // PCR_Read
    0x181, // ReadClock
    0x18A, // TestParms
];

/// Whether the bytes of the command with `header` (and of its response)
/// may be logged or put in a crash report.
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(code = ?header.map(|h| h.code)))]
pub fn bytes_are_public(header: Option<CommandHeader>) -> bool {
    header.is_some_and(|h| h.tag == ST_NO_SESSIONS && PUBLIC_COMMANDS.contains(&h.code))
}

/// `TPM2_<name>` for a command code, when it is one Ternvale knows.
#[tracing::instrument(level = "debug", target = "ternvale::tpm", skip_all, fields(code = %format!("{code:#x}")))]
pub fn command_name(code: u32) -> Option<&'static str> {
    let name = match code {
        0x11F => "NV_UndefineSpaceSpecial",
        0x120 => "EvictControl",
        0x121 => "HierarchyControl",
        0x122 => "NV_UndefineSpace",
        0x124 => "ChangeEPS",
        0x125 => "ChangePPS",
        0x126 => "Clear",
        0x127 => "ClearControl",
        0x128 => "ClockSet",
        0x129 => "HierarchyChangeAuth",
        0x12A => "NV_DefineSpace",
        0x12B => "PCR_Allocate",
        0x12C => "PCR_SetAuthPolicy",
        0x12D => "PP_Commands",
        0x12E => "SetPrimaryPolicy",
        0x130 => "ClockRateAdjust",
        0x131 => "CreatePrimary",
        0x132 => "NV_GlobalWriteLock",
        0x133 => "GetCommandAuditDigest",
        0x134 => "NV_Increment",
        0x135 => "NV_SetBits",
        0x136 => "NV_Extend",
        0x137 => "NV_Write",
        0x138 => "NV_WriteLock",
        0x139 => "DictionaryAttackLockReset",
        0x13A => "DictionaryAttackParameters",
        0x13B => "NV_ChangeAuth",
        0x13C => "PCR_Event",
        0x13D => "PCR_Reset",
        0x13E => "SequenceComplete",
        0x142 => "IncrementalSelfTest",
        0x143 => "SelfTest",
        0x144 => "Startup",
        0x145 => "Shutdown",
        0x146 => "StirRandom",
        0x147 => "ActivateCredential",
        0x148 => "Certify",
        0x149 => "PolicyNV",
        0x14A => "CertifyCreation",
        0x14B => "Duplicate",
        0x14C => "GetTime",
        0x14D => "GetSessionAuditDigest",
        0x14E => "NV_Read",
        0x14F => "NV_ReadLock",
        0x150 => "ObjectChangeAuth",
        0x151 => "PolicySecret",
        0x152 => "Rewrap",
        0x153 => "Create",
        0x154 => "ECDH_ZGen",
        0x155 => "HMAC",
        0x156 => "Import",
        0x157 => "Load",
        0x158 => "Quote",
        0x159 => "RSA_Decrypt",
        0x15B => "HMAC_Start",
        0x15C => "SequenceUpdate",
        0x15D => "Sign",
        0x15E => "Unseal",
        0x160 => "PolicySigned",
        0x161 => "ContextLoad",
        0x162 => "ContextSave",
        0x163 => "ECDH_KeyGen",
        0x164 => "EncryptDecrypt",
        0x165 => "FlushContext",
        0x167 => "LoadExternal",
        0x168 => "MakeCredential",
        0x169 => "NV_ReadPublic",
        0x16A => "PolicyAuthorize",
        0x16B => "PolicyAuthValue",
        0x16C => "PolicyCommandCode",
        0x16D => "PolicyCounterTimer",
        0x16E => "PolicyCpHash",
        0x16F => "PolicyLocality",
        0x170 => "PolicyNameHash",
        0x171 => "PolicyOR",
        0x172 => "PolicyTicket",
        0x173 => "ReadPublic",
        0x174 => "RSA_Encrypt",
        0x176 => "StartAuthSession",
        0x177 => "VerifySignature",
        0x178 => "ECC_Parameters",
        0x17A => "GetCapability",
        0x17B => "GetRandom",
        0x17C => "GetTestResult",
        0x17D => "Hash",
        0x17E => "PCR_Read",
        0x17F => "PolicyPCR",
        0x180 => "PolicyRestart",
        0x181 => "ReadClock",
        0x182 => "PCR_Extend",
        0x183 => "PCR_SetAuthValue",
        0x184 => "NV_Certify",
        0x185 => "EventSequenceComplete",
        0x186 => "HashSequenceStart",
        0x189 => "PolicyGetDigest",
        0x18A => "TestParms",
        0x18B => "Commit",
        0x18C => "PolicyPassword",
        0x191 => "CreateLoaded",
        0x193 => "EncryptDecrypt2",
        _ => return None,
    };
    Some(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::ST_SESSIONS;

    fn header(tag: u16, code: u32) -> Option<CommandHeader> {
        Some(CommandHeader {
            tag,
            size: 10,
            code,
        })
    }

    #[test]
    fn only_listed_commands_without_sessions_are_public() {
        assert!(bytes_are_public(header(ST_NO_SESSIONS, 0x144)));
        assert!(bytes_are_public(header(ST_NO_SESSIONS, 0x17A)));
        assert!(
            !bytes_are_public(header(ST_SESSIONS, 0x17A)),
            "session area"
        );
        for secret in [
            0x17B, 0x131, 0x153, 0x157, 0x15E, 0x14E, 0x137, 0x159, 0x176,
        ] {
            assert!(
                !bytes_are_public(header(ST_NO_SESSIONS, secret)),
                "{secret:#x} ({:?})",
                command_name(secret)
            );
        }
        assert!(!bytes_are_public(None), "unparsable header");
    }

    #[test]
    fn every_public_command_has_a_name() {
        for code in PUBLIC_COMMANDS {
            assert!(command_name(code).is_some(), "{code:#x}");
        }
        assert_eq!(command_name(0x17B), Some("GetRandom"));
        assert_eq!(command_name(0x144), Some("Startup"));
        assert_eq!(command_name(0xdead), None);
    }
}
