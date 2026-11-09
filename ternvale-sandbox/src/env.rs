//! Process roles: launcher (can spawn swtpm) vs sandboxed VMM.

/// Set on the Seatbelt child; the VMM must not spawn swtpm when this is `1`.
pub const SANDBOXED_ENV: &str = "TERNVALE_SANDBOXED";
/// Skip Seatbelt (`ternvale run` tests, `scripts/sign-and-run.sh` guests).
pub const SKIP_ENV: &str = "TERNVALE_SKIP_SANDBOX";
/// Code Signing Language string the privileged helper requires of callers.
pub const REQUIREMENT_ENV: &str = "TERNVALE_HELPER_REQUIREMENT";
/// Default caller requirement: the ad-hoc CLI identifier.
pub const DEFAULT_CLIENT_REQUIREMENT: &str = r#"identifier "ternvale""#;
/// Allow unsigned helper clients (debug only).
pub const ALLOW_UNSIGNED_ENV: &str = "TERNVALE_HELPER_ALLOW_UNSIGNED";

/// True when this process is the Seatbelt-confined VMM.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn is_sandboxed() -> bool {
    let yes = std::env::var(SANDBOXED_ENV).is_ok_and(|v| v == "1");
    tracing::debug!(target: "ternvale::sandbox", sandboxed = yes, "sandbox role");
    yes
}

/// True when the operator asked not to apply Seatbelt.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn skip_sandbox() -> bool {
    let skip = std::env::var(SKIP_ENV).is_ok_and(|v| v == "1");
    if skip {
        tracing::warn!(
            target: "ternvale::sandbox",
            env = SKIP_ENV,
            "Seatbelt skipped; VMM is not confined"
        );
    }
    skip
}

/// Only the unsandboxed launcher may `exec` swtpm.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn may_spawn_swtpm() -> bool {
    let allowed = !is_sandboxed();
    if !allowed {
        tracing::warn!(
            target: "ternvale::sandbox",
            "sandboxed VMM refused to spawn swtpm; the launcher must start it"
        );
    }
    allowed
}

/// Requirement string the helper uses for `SecCodeCheckValidity`.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn helper_requirement() -> String {
    std::env::var(REQUIREMENT_ENV).unwrap_or_else(|_| DEFAULT_CLIENT_REQUIREMENT.to_string())
}

/// Debug escape hatch: helper skips codesign (never in production docs).
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn helper_allows_unsigned() -> bool {
    std::env::var(ALLOW_UNSIGNED_ENV).is_ok_and(|v| v == "1")
}

#[cfg(test)]
mod tests {
    use super::{is_sandboxed, may_spawn_swtpm, SANDBOXED_ENV};

    #[test]
    fn launcher_may_spawn_swtpm() {
        // This unit test binary is not the Seatbelt child unless a caller set the env.
        if std::env::var(SANDBOXED_ENV).is_ok_and(|v| v == "1") {
            return;
        }
        assert!(!is_sandboxed());
        assert!(may_spawn_swtpm());
    }
}
