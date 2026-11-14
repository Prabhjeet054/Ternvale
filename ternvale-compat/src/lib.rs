//! x86/x86_64 usermode engines for Linux guests.
//!
//! Selection uses [`EngineCaps`] (from [`ternvale_backend::BackendCapabilities`]
//! in callers). There is no `if backend == "vz"` here.

use ternvale_config::{Profile, X86Emulation};

/// Capability bits a backend advertises for x86 translation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct EngineCaps {
    /// Apple `VZLinuxRosettaDirectoryShare` (Virtualization.framework Linux).
    pub rosetta: bool,
    /// FEX-Emu in the guest (built for aarch64 Linux; not bundled RootFS).
    pub fex: bool,
    /// Distro qemu-user-static (not bundled; GPL; Step 99 packages).
    pub qemu_user: bool,
}

/// Resolved engine written to `ternvale status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum X86Engine {
    None,
    Rosetta,
    Fex,
    QemuUser,
}

impl X86Engine {
    #[tracing::instrument(level = "trace", target = "ternvale::compat", skip_all)]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Rosetta => "rosetta",
            Self::Fex => "fex",
            Self::QemuUser => "qemu-user",
        }
    }
}

impl std::fmt::Display for X86Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Why [`resolve`] refused a requested engine.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompatError {
    #[error("x86_emulation = \"{requested}\" is not supported ({capability})")]
    Unsupported {
        requested: &'static str,
        capability: &'static str,
    },
    #[error("x86_emulation = \"{requested}\" needs a Linux guest")]
    NotLinux { requested: &'static str },
}

/// Combine `x86_emulation` with legacy `rosetta = true`.
#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
pub fn requested(x86: X86Emulation, rosetta_flag: bool) -> X86Emulation {
    match x86 {
        X86Emulation::None if rosetta_flag => X86Emulation::Rosetta,
        X86Emulation::Auto if rosetta_flag => X86Emulation::Rosetta,
        other => other,
    }
}

/// Pick an engine. `auto` prefers Rosetta, then FEX, then qemu-user.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::compat",
    skip_all,
    fields(profile = %profile, requested = %want, rosetta = caps.rosetta, fex = caps.fex)
)]
pub fn resolve(
    want: X86Emulation,
    caps: EngineCaps,
    profile: Profile,
) -> Result<X86Engine, CompatError> {
    let linux = profile.is_linux();
    let engine = match want {
        X86Emulation::None => X86Engine::None,
        X86Emulation::Auto => {
            if linux && caps.rosetta {
                X86Engine::Rosetta
            } else if linux && caps.fex {
                X86Engine::Fex
            } else if linux && caps.qemu_user {
                X86Engine::QemuUser
            } else {
                X86Engine::None
            }
        }
        X86Emulation::Rosetta => X86Engine::Rosetta,
        X86Emulation::Fex => X86Engine::Fex,
        X86Emulation::QemuUser => X86Engine::QemuUser,
    };
    if matches!(
        engine,
        X86Engine::Rosetta | X86Engine::Fex | X86Engine::QemuUser
    ) && !linux
    {
        return Err(CompatError::NotLinux {
            requested: engine.as_str(),
        });
    }
    let ok = match engine {
        X86Engine::None => true,
        X86Engine::Rosetta => caps.rosetta,
        X86Engine::Fex => caps.fex,
        X86Engine::QemuUser => caps.qemu_user,
    };
    if !ok {
        tracing::warn!(
            target: "ternvale::compat",
            requested = engine.as_str(),
            "x86 engine not advertised by this backend"
        );
        return Err(CompatError::Unsupported {
            requested: engine.as_str(),
            capability: engine.as_str(),
        });
    }
    tracing::info!(
        target: "ternvale::compat",
        engine = engine.as_str(),
        "resolved x86 emulation engine"
    );
    Ok(engine)
}

/// Caps for the native Hypervisor.framework backend (no Apple Linux Rosetta).
#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
pub fn native_caps() -> EngineCaps {
    EngineCaps {
        rosetta: false,
        fex: true,
        qemu_user: true,
    }
}

/// Caps for VZ Linux (`VZLinuxRosettaDirectoryShare` exists; FEX still works).
#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
pub fn linux_vz_caps() -> EngineCaps {
    EngineCaps {
        rosetta: true,
        fex: true,
        qemu_user: true,
    }
}

/// Caps for VZ macOS guests (no Linux usermode x86 stack).
#[tracing::instrument(level = "debug", target = "ternvale::compat", skip_all)]
pub fn macos_vz_caps() -> EngineCaps {
    EngineCaps::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ternvale_config::{Profile, X86Emulation};

    #[test]
    fn auto_native_picks_fex() {
        let engine = resolve(X86Emulation::Auto, native_caps(), Profile::Linux).unwrap();
        assert_eq!(engine, X86Engine::Fex);
        assert_eq!(engine.as_str(), "fex");
    }

    #[test]
    fn auto_linux_vz_picks_rosetta() {
        let engine = resolve(X86Emulation::Auto, linux_vz_caps(), Profile::Linux).unwrap();
        assert_eq!(engine, X86Engine::Rosetta);
    }

    #[test]
    fn native_refuses_rosetta() {
        let err = resolve(X86Emulation::Rosetta, native_caps(), Profile::Linux).unwrap_err();
        assert!(matches!(
            err,
            CompatError::Unsupported {
                capability: "rosetta",
                ..
            }
        ));
    }

    #[test]
    fn macos_auto_is_none() {
        let engine = resolve(X86Emulation::Auto, macos_vz_caps(), Profile::Macos).unwrap();
        assert_eq!(engine, X86Engine::None);
    }

    #[test]
    fn fex_on_macos_fails() {
        let err = resolve(X86Emulation::Fex, macos_vz_caps(), Profile::Macos).unwrap_err();
        assert!(matches!(err, CompatError::NotLinux { .. }));
    }

    #[test]
    fn rosetta_flag_upgrades_none() {
        assert_eq!(requested(X86Emulation::None, true), X86Emulation::Rosetta);
        assert_eq!(requested(X86Emulation::Fex, true), X86Emulation::Fex);
    }
}
