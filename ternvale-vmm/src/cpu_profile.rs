//! CPU feature fingerprints for portable snapshots.
//!
//! `hv_vcpu_config.h` has `hv_vcpu_config_get_feature_reg` and **no** setter.
//! `baseline` ANDs optional ID_AA64* fields then tries `hv_vcpu_set_sys_reg`.
//! Restore compares the snapshot's observed IDs to this host's get_feature_reg
//! values and lists every nibble the snapshot needs that the host cannot
//! provide.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};
use ternvale_config::CpuProfile;
use ternvale_hv::{read_host_features, FeatureReg};

use crate::vcpu::{Vcpu, VcpuError};

/// On-disk / in-memory ID register set plus the profile name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct CpuFingerprint {
    /// `host` or `baseline`.
    pub profile: String,
    /// ARM register name → raw value.
    pub id_registers: BTreeMap<String, u64>,
}

/// One ID field the snapshot used that this host does not advertise.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuMismatch {
    /// `ID_AA64…` name.
    pub reg: String,
    /// 4-bit field index (0 = bits \[3:0\]).
    pub nibble: u8,
    /// Snapshot guest value (0–15).
    pub snapshot: u8,
    /// Host `get_feature_reg` value (0–15), or `None` if unread.
    pub host: Option<u8>,
}

impl CpuMismatch {
    /// One line for CLI / control-socket errors.
    #[must_use]
    pub fn describe(&self) -> String {
        match self.host {
            Some(host) => format!(
                "{} nibble {} (bits [{}:{}]): snapshot {:#x}, host {:#x}",
                self.reg,
                self.nibble,
                self.nibble * 4 + 3,
                self.nibble * 4,
                self.snapshot,
                host
            ),
            None => format!(
                "{} nibble {} (bits [{}:{}]): snapshot {:#x}, host unavailable",
                self.reg,
                self.nibble,
                self.nibble * 4 + 3,
                self.nibble * 4,
                self.snapshot
            ),
        }
    }
}

/// Decide whether a snapshot's observed IDs fit `host` (injected in tests).
///
/// Logs WARN and returns `Ok` when `force` is set despite mismatches.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cpuprofile",
    skip_all,
    fields(force, profile = %snapshot.profile)
)]
pub fn evaluate_restore(
    snapshot: &CpuFingerprint,
    host: &CpuFingerprint,
    force: bool,
) -> Result<(), String> {
    if snapshot.id_registers.is_empty() {
        tracing::warn!(
            target: "ternvale::cpuprofile",
            "snapshot has no CPU fingerprint; skipping ID check"
        );
        return Ok(());
    }
    let miss = mismatches(snapshot, host);
    if miss.is_empty() {
        tracing::debug!(
            target: "ternvale::cpuprofile",
            profile = snapshot.profile.as_str(),
            "snapshot CPU fingerprint fits this host"
        );
        return Ok(());
    }
    let detail = format_mismatch_error(&miss);
    if force {
        tracing::warn!(
            target: "ternvale::cpuprofile",
            mismatches = miss.len(),
            "restoring snapshot despite CPU feature mismatch (--force)"
        );
        for row in &miss {
            tracing::warn!(
                target: "ternvale::cpuprofile",
                mismatch = %row.describe(),
                "CPU feature mismatch"
            );
        }
        return Ok(());
    }
    tracing::error!(target: "ternvale::cpuprofile", error = %detail, "snapshot restore refused");
    Err(detail)
}

/// Readable multi-line restore error.
#[tracing::instrument(level = "debug", target = "ternvale::cpuprofile", skip_all)]
pub fn format_mismatch_error(mismatches: &[CpuMismatch]) -> String {
    let mut text = String::from(
        "refusing snapshot restore: this Mac cannot provide CPU features the guest observed:\n",
    );
    for row in mismatches {
        let _ = writeln!(text, "  {}", row.describe());
    }
    text.push_str("re-run with --force to restore anyway (guest may UNDEF on missing features)");
    text
}

/// Compare snapshot IDs to a host map (tests inject maps; live uses [`host_fingerprint`]).
#[tracing::instrument(level = "debug", target = "ternvale::cpuprofile", skip_all)]
pub fn mismatches(snapshot: &CpuFingerprint, host: &CpuFingerprint) -> Vec<CpuMismatch> {
    let mut out = Vec::new();
    for (name, &guest) in &snapshot.id_registers {
        let host_val = host.id_registers.get(name).copied();
        for nibble in 0u8..16 {
            let g = nibble_of(guest, nibble);
            if !feature_present(g) {
                continue;
            }
            let h = host_val.map(|v| nibble_of(v, nibble));
            if !matches!(h, Some(hv) if feature_present(hv) && hv >= g) {
                out.push(CpuMismatch {
                    reg: name.clone(),
                    nibble,
                    snapshot: g,
                    host: h,
                });
            }
        }
    }
    out
}

/// ARM ID convention: 0 = not implemented; 0xF is often RES0 / IMPDEF unused.
fn feature_present(nibble: u8) -> bool {
    nibble != 0 && nibble != 0xf
}

fn nibble_of(value: u64, nibble: u8) -> u8 {
    ((value >> (u32::from(nibble) * 4)) & 0xf) as u8
}

/// `hv_vcpu_config_get_feature_reg` dump for this Mac.
#[tracing::instrument(level = "debug", target = "ternvale::cpuprofile", skip_all)]
pub fn host_fingerprint() -> Result<CpuFingerprint, String> {
    let rows = read_host_features().map_err(|error| error.to_string())?;
    let mut id_registers = BTreeMap::new();
    for row in rows {
        if let Some(value) = row.value {
            id_registers.insert(row.reg.name().to_string(), value);
        }
    }
    Ok(CpuFingerprint {
        profile: "host".into(),
        id_registers,
    })
}

/// Apply `baseline` masks via `hv_vcpu_set_sys_reg`, then snapshot what the guest will see.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cpuprofile",
    skip_all,
    fields(vcpu_id = vcpu.id(), profile = %profile)
)]
pub fn apply_and_capture(vcpu: &Vcpu, profile: CpuProfile) -> Result<CpuFingerprint, VcpuError> {
    let mut id_registers = BTreeMap::new();
    for reg in ID_REGS {
        let Some(enc) = reg.sys_reg_encoding() else {
            continue;
        };
        let before = match vcpu.get_sys_reg_enc(enc) {
            Ok(v) => v,
            Err(error) => {
                tracing::debug!(
                    target: "ternvale::cpuprofile",
                    reg = reg.name(),
                    error = %error,
                    "ID register unreadable"
                );
                continue;
            }
        };
        let want = match profile {
            CpuProfile::Host => before,
            CpuProfile::Baseline => baseline_mask(reg, before),
        };
        if want != before {
            match vcpu.set_sys_reg_enc(enc, want) {
                Ok(()) => tracing::debug!(
                    target: "ternvale::cpuprofile",
                    reg = reg.name(),
                    before = format!("{before:#x}"),
                    after = format!("{want:#x}"),
                    "masked ID register for baseline profile"
                ),
                Err(error) => {
                    tracing::warn!(
                        target: "ternvale::cpuprofile",
                        reg = reg.name(),
                        error = %error,
                        "Hypervisor.framework rejected ID register mask; keeping host value"
                    );
                }
            }
        }
        let observed = vcpu.get_sys_reg_enc(enc).unwrap_or(before);
        tracing::debug!(
            target: "ternvale::cpuprofile",
            reg = reg.name(),
            value = format!("{observed:#x}"),
            "guest-visible ID register"
        );
        id_registers.insert(reg.name().to_string(), observed);
    }
    Ok(CpuFingerprint {
        profile: profile.as_str().to_string(),
        id_registers,
    })
}

/// Persist `cpu.json` beside the serial log (typically the VM library dir).
#[tracing::instrument(
    level = "debug",
    target = "ternvale::cpuprofile",
    skip_all,
    fields(dir = %dir.display())
)]
pub fn write_cpu_json(dir: &Path, fp: &CpuFingerprint) -> Result<(), String> {
    let path = dir.join("cpu.json");
    let json = serde_json::to_vec_pretty(fp).map_err(|error| error.to_string())?;
    std::fs::write(&path, json).map_err(|error| format!("{}: {error}", path.display()))?;
    tracing::info!(
        target: "ternvale::cpuprofile",
        path = %path.display(),
        profile = %fp.profile,
        regs = fp.id_registers.len(),
        "wrote VM cpu.json fingerprint"
    );
    Ok(())
}

/// Hex map for `meta.json`.
#[must_use]
pub fn to_hex_map(fp: &CpuFingerprint) -> BTreeMap<String, String> {
    fp.id_registers
        .iter()
        .map(|(k, v)| (k.clone(), format!("{v:#x}")))
        .collect()
}

/// Parse hex map from `meta.json`.
#[must_use]
pub fn from_hex_map(profile: Option<&str>, map: &BTreeMap<String, String>) -> CpuFingerprint {
    let mut id_registers = BTreeMap::new();
    for (k, v) in map {
        if let Ok(n) = parse_u64(v) {
            id_registers.insert(k.clone(), n);
        }
    }
    CpuFingerprint {
        profile: profile.unwrap_or("host").to_string(),
        id_registers,
    }
}

fn parse_u64(text: &str) -> Result<u64, std::num::ParseIntError> {
    let t = text.trim();
    if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
        u64::from_str_radix(hex, 16)
    } else {
        t.parse()
    }
}

/// Optional extensions that differ across Apple chips (ARM ARM ID field layout).
/// TODO(verify) against M1/M2/M4 ID dumps on other machines.
fn baseline_mask(reg: FeatureReg, value: u64) -> u64 {
    match reg {
        // ID_AA64PFR0_EL1 SVE [35:32], RAS [31:28] kept; GIC [27:24] kept.
        FeatureReg::IdAa64Pfr0El1 => value & !nibble_mask(8),
        // ID_AA64PFR1_EL1 SME [27:24], MTE [11:8] cleared for a common subset.
        FeatureReg::IdAa64Pfr1El1 => value & !(nibble_mask(6) | nibble_mask(2)),
        FeatureReg::IdAa64Zfr0El1 | FeatureReg::IdAa64Smfr0El1 => 0,
        FeatureReg::IdAa64Isar2El1
        | FeatureReg::IdAa64Pfr2El1
        | FeatureReg::IdAa64Mmfr3El1
        | FeatureReg::IdAa64Mmfr4El1 => 0,
        _ => value,
    }
}

fn nibble_mask(nibble: u32) -> u64 {
    0xf << (nibble * 4)
}

#[cfg(test)]
#[path = "cpu_profile_tests.rs"]
mod tests;

const ID_REGS: [FeatureReg; 15] = [
    FeatureReg::IdAa64Dfr0El1,
    FeatureReg::IdAa64Dfr1El1,
    FeatureReg::IdAa64Isar0El1,
    FeatureReg::IdAa64Isar1El1,
    FeatureReg::IdAa64Isar2El1,
    FeatureReg::IdAa64Mmfr0El1,
    FeatureReg::IdAa64Mmfr1El1,
    FeatureReg::IdAa64Mmfr2El1,
    FeatureReg::IdAa64Mmfr3El1,
    FeatureReg::IdAa64Mmfr4El1,
    FeatureReg::IdAa64Pfr0El1,
    FeatureReg::IdAa64Pfr1El1,
    FeatureReg::IdAa64Pfr2El1,
    FeatureReg::IdAa64Smfr0El1,
    FeatureReg::IdAa64Zfr0El1,
];
