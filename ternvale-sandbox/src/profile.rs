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
/// TODO(verify): Virtualization.framework (`VZVirtualMachine.start`) under the
/// same profile when `network_client` is set for VZ NAT (Step 92).
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all, fields(grants = grants.paths.len(), network_client = grants.network_client))]
pub fn render(grants: &Grants) -> String {
    render_for_child(grants, None)
}

/// Same as [`render`]. `child` is accepted for the Seatbelt relaunch path.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn render_for_child(grants: &Grants, child: Option<&Path>) -> String {
    let mut out = String::from("(version 1)\n(deny default)\n");
    out.push_str("; Ternvale per-VM VMM. Privileged ops stay in ternvale-vmnetd.\n");
    // Rust/Hypervisor need process*/sysctl*/mach*/system* under `(deny default)`
    // on macOS 15–26; a lone `(allow process-exec (literal …))` still EPERM.
    let _ = child;
    out.push_str("(allow process*)\n(allow signal)\n(allow sysctl*)\n(allow system*)\n");
    out.push_str("(allow file-ioctl)\n(allow file-read-metadata)\n(allow file-map-executable)\n");
    out.push_str("(allow mach*)\n(allow iokit-open)\n");
    out.push_str("(allow ipc-posix*)\n(allow system-socket)\n");
    out.push_str("(allow file-read* (subpath \"/usr/lib\") (subpath \"/System\") (subpath \"/usr/share\") (subpath \"/Library/Developer\"))\n");
    out.push_str("(allow file-read* (literal \"/dev/null\") (literal \"/dev/urandom\") (literal \"/dev/dtracehelper\") (literal \"/dev/ptmx\"))\n");
    out.push_str("(allow file-read* file-write* (regex #\"^/dev/ttys\"))\n");
    if grants.network_client {
        out.push_str("(allow network-outbound)\n(allow network-bind)\n(allow network-inbound)\n");
    } else {
        out.push_str("; no user-mode NAT: deny IP networking (unix sockets still allowed below)\n");
        out.push_str("(deny network-outbound)\n(deny network-bind)\n");
    }
    // TODO(verify): Apple SBPL `device-microphone` / `device-camera` vs App
    // Sandbox `com.apple.security.device.audio-input` / `device.camera` on
    // macOS 15–26. Grant only when the VM opted in; TCC still prompts.
    if grants.microphone {
        out.push_str("(allow device-microphone)\n");
    }
    if grants.camera {
        out.push_str("(allow device-camera)\n");
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
            // `(file-read* (subpath …))` without `allow` is `sandbox-exec: illegal function`.
            let _ = writeln!(out, "(allow {access} (subpath \"{path}\"))");
        } else {
            let _ = writeln!(out, "(allow {access} (literal \"{path}\"))");
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
        assert!(text.contains("(deny default)"));
        assert!(text.contains("(allow process*)"));
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
        assert!(
            text.contains("(allow process*)"),
            "sandbox-exec must be allowed to start the VMM binary"
        );
    }

    #[test]
    fn path_grants_use_allow_and_sandbox_exec_parses() {
        let config = VmConfig::parse(
            "name = \"p\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n",
        )
        .unwrap();
        let grants = Grants::from_config(&config, std::path::Path::new("/tmp/vm.toml"));
        let text = render(&grants);
        assert!(
            text.contains("(allow file-read*"),
            "path grants must be allow rules\n{text}"
        );
        assert!(
            !text
                .lines()
                .any(|line| line.starts_with("(file-read") || line.starts_with("(file-write")),
            "bare file-* filters are illegal SBPL functions\n{text}"
        );
        let dir = std::env::temp_dir().join(format!("tv-sb-parse-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("vmm.sb");
        std::fs::write(&path, &text).unwrap();
        let out = std::process::Command::new("/usr/bin/sandbox-exec")
            .arg("-f")
            .arg(&path)
            .arg("/usr/bin/true")
            .output()
            .expect("sandbox-exec");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(
            !err.contains("illegal function"),
            "sandbox-exec rejected profile: {err}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn microphone_opt_in_emits_device_rule() {
        let config = VmConfig::parse(
            "name = \"p\"\ncpus = 1\nram_mib = 256\nkernel = \"/k\"\nserial_log = \"/tmp/s.log\"\n[audio]\ninput = true\n",
        )
        .unwrap();
        let grants = Grants::from_config(&config, std::path::Path::new("/tmp/vm.toml"));
        let text = render(&grants);
        assert!(text.contains("(allow device-microphone)"), "{text}");
        assert!(!text.contains("(allow device-camera)"), "{text}");
    }
}
