//! Guest x86/x86_64 usermode emulation (`x86_emulation` in the VM config).

use serde::{Deserialize, Serialize};

/// How a Linux guest should run Intel binaries.
///
/// Rosetta is **only** Apple's `VZLinuxRosettaDirectoryShare` on the VZ
/// backend. Hypervisor.framework (native) has no equivalent API; Ternvale
/// never extracts or reuses Rosetta outside that mechanism.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum X86Emulation {
    /// Do not set up x86 translation (unless `rosetta = true`).
    #[default]
    None,
    /// Apple Linux Rosetta share (VZ Linux only).
    Rosetta,
    /// FEX-Emu in the guest (native Linux; also usable on VZ Linux).
    Fex,
    /// Distro `qemu-user-static` in the guest (not bundled; Step 99).
    QemuUser,
    /// Pick the best engine the backend advertises.
    Auto,
}

impl X86Emulation {
    /// Config / status spelling.
    /// True for the omitted TOML default.
    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }

    #[tracing::instrument(level = "trace", target = "ternvale::config", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Rosetta => "rosetta",
            Self::Fex => "fex",
            Self::QemuUser => "qemu-user",
            Self::Auto => "auto",
        }
    }
}

impl std::fmt::Display for X86Emulation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::X86Emulation;

    #[test]
    fn toml_round_trip_spellings() {
        for (text, want) in [
            ("none", X86Emulation::None),
            ("rosetta", X86Emulation::Rosetta),
            ("fex", X86Emulation::Fex),
            ("qemu-user", X86Emulation::QemuUser),
            ("auto", X86Emulation::Auto),
        ] {
            let got: X86Emulation = toml::from_str(&format!("v = \"{text}\"\n"))
                .map(|w: Wrapper| w.v)
                .unwrap();
            assert_eq!(got, want);
            assert_eq!(want.as_str(), text);
        }
    }

    #[derive(serde::Deserialize)]
    struct Wrapper {
        v: X86Emulation,
    }
}
