//! Seatbelt (sandbox_init) profile for the per-VM VMM.

use std::fmt::Write as _;
use std::path::Path;

use super::grants::Grants;

/// `sandbox-exec` binary.
pub const SANDBOX_EXEC: &str = "/usr/bin/sandbox-exec";

/// Render a `(version 1)` profile for `grants`.
///
/// TODO(verify): Hypervisor.framework (`hv_vm_create`) under this profile on
/// macOS 15, 16, and 26. If `hv_vm_create` returns `HV_DENIED`, widen
/// `mach-lookup` / `iokit-open` here rather than granting general file access.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all, fields(grants = grants.paths.len(), network_client = grants.network_client))]
pub fn render(grants: &Grants) -> String {
    let mut out = String::from("(version 1)\n(deny default)\n");
    out.push_str("; Ternvale per-VM VMM. Privileged ops stay in ternvale-vmnetd.\n");
    out.push_str("(deny process-exec)\n(deny process-fork)\n");
    out.push_str("(allow process-info*)\n(allow signal)\n(allow sysctl-read)\n");
    out.push_str("(allow file-ioctl)\n(allow file-read-metadata)\n");
    out.push_str("(allow mach-lookup)\n(allow mach-register)\n(allow iokit-open)\n");
    out.push_str("(allow ipc-posix-shm)\n(allow system-socket)\n");
    out.push_str("(allow file-read* (subpath \"/usr/lib\") (subpath \"/System\") (subpath \"/usr/share\") (subpath \"/Library/Developer\"))\n");
    out.push_str("(allow file-read* (literal \"/dev/null\") (literal \"/dev/urandom\") (literal \"/dev/dtracehelper\") (literal \"/dev/ptmx\"))\n");
    out.push_str("(allow file-read* file-write* (regex #\"^/dev/ttys\"))\n");
    if grants.network_client {
        out.push_str("(allow network-outbound)\n(allow network-bind)\n(allow network-inbound)\n");
    } else {
        out.push_str("; no user-mode NAT: deny IP networking (unix sockets still allowed below)\n");
        out.push_str("(deny network-outbound)\n(deny network-bind)\n");
    }
    out.push_str("(allow network-outbound (remote unix))\n");
    out.push_str("(allow network-inbound (local unix))\n");
    for grant in &grants.paths {
        let path = sb_literal(&grant.path);
        let access = if grant.write {
            "file-read* file-write*"
        } else {
            "file-read*"
        };
        if grant.directory {
            let _ = writeln!(out, "({access} (subpath \"{path}\"))");
        } else {
            let _ = writeln!(out, "({access} (literal \"{path}\"))");
            if let Some(parent) = grant.path.parent() {
                let parent = sb_literal(parent);
                let _ = writeln!(out, "(allow file-read* (subpath \"{parent}\"))");
            }
        }
    }
    tracing::debug!(
        target: "ternvale::sandbox",
        bytes = out.len(),
        "rendered seatbelt profile"
    );
    out
}

fn sb_literal(path: &Path) -> String {
    path.to_string_lossy()
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::render;
    use crate::grants::Grants;
    use ternvale_config::VmConfig;

    #[test]
    fn profile_denies_exec_and_omits_network_for_vmnet() {
        let config = VmConfig::parse(
            "name = \"p\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n[[nics]]\nbackend = \"vmnet\"\n",
        )
        .unwrap();
        let grants = Grants::from_config(&config, std::path::Path::new("/tmp/vm.toml"));
        let text = render(&grants);
        assert!(text.contains("(deny process-exec)"));
        assert!(text.contains("(deny network-outbound)"));
        assert!(!text.contains("(allow network-outbound)\n(allow network-bind)"));
    }

    #[test]
    fn user_nat_allows_network_client() {
        let config = VmConfig::parse(
            "name = \"p\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n[[nics]]\nbackend = \"user\"\n",
        )
        .unwrap();
        let grants = Grants::from_config(&config, std::path::Path::new("/tmp/vm.toml"));
        let text = render(&grants);
        assert!(text.contains("(allow network-outbound)"));
    }
}
