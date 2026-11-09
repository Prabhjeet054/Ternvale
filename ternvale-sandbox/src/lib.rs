//! Seatbelt confinement for the per-VM Ternvale helper.
//!
//! The unsandboxed launcher builds a minimal profile, writes security-scoped
//! bookmarks for the VM bundle / ISOs / shares, and starts `sandbox-exec`.
//! Privileged work (vmnet, USB claim) stays in `ternvale-vmnetd`.

mod bookmarks;
mod env;
mod error;
mod grants;
mod launch;
mod probe;
mod profile;

pub use bookmarks::{
    parse_store, start_access, write_store, MAGIC as BOOKMARK_MAGIC, VERSION as BOOKMARK_VERSION,
};
pub use env::{
    helper_allows_unsigned, helper_requirement, is_sandboxed, may_spawn_swtpm, skip_sandbox,
    ALLOW_UNSIGNED_ENV, DEFAULT_CLIENT_REQUIREMENT, REQUIREMENT_ENV, SANDBOXED_ENV, SKIP_ENV,
};
pub use error::SandboxError;
pub use grants::{Grant, Grants};
pub use launch::{hold_bookmarks, maybe_relaunch, BOOKMARKS_ENV};
pub use probe::{
    probe_requested, run_debug_probe, ProbeItem, ProbeReport, PROBE_CHILD_ENV, PROBE_ENV,
};
pub use profile::{render, SANDBOX_EXEC};

/// Helper caller check: peer of `fd` must satisfy the codesign requirement.
#[tracing::instrument(level = "debug", target = "ternvale::sandbox", skip_all)]
pub fn authorize_helper_client(fd: std::os::fd::RawFd) -> Result<(), SandboxError> {
    if helper_allows_unsigned() {
        tracing::warn!(
            target: "ternvale::sandbox",
            "privileged helper accepted an unchecked client ({})",
            ALLOW_UNSIGNED_ENV
        );
        return Ok(());
    }
    let pid = ternvale_hv::unix_peer_pid(fd).map_err(|source| SandboxError::Io {
        op: "LOCAL_PEERPID",
        source,
    })?;
    let requirement = helper_requirement();
    ternvale_hv::check_pid_requirement(pid, &requirement).map_err(SandboxError::Sec)?;
    tracing::info!(
        target: "ternvale::sandbox",
        pid,
        requirement = %requirement,
        "privileged helper accepted signed caller"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::os::fd::AsRawFd;
    use std::os::unix::net::UnixListener;
    use std::process::{Command, Stdio};

    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-sandbox");
    }

    #[test]
    fn helper_rejects_wrong_identity() {
        let (a, b) = std::os::unix::net::UnixStream::pair().expect("pair");
        let pid = ternvale_hv::unix_peer_pid(a.as_raw_fd()).expect("peer pid");
        let err = ternvale_hv::check_pid_requirement(pid, r#"identifier "com.apple.Safari""#)
            .expect_err("Safari requirement must not match this test binary");
        tracing::info!(target: "ternvale::sandbox", error = %err, "wrong-identity client rejected");
        drop(b);
    }

    #[test]
    fn helper_rejects_unsigned_or_non_ternvale_client() {
        let dir = std::env::temp_dir().join(format!("tv-helper-auth-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        let sock = dir.join("helper.sock");
        let _ = std::fs::remove_file(&sock);
        let listener = UnixListener::bind(&sock).expect("bind");
        let sock_str = sock.display().to_string();
        let mut child = Command::new("/usr/bin/python3")
            .args([
                "-c",
                "import socket,sys,time; s=socket.socket(socket.AF_UNIX); time.sleep(0.2); s.connect(sys.argv[1]); s.send(b'TVNO'); time.sleep(1)",
                &sock_str,
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("python3 client");
        listener.set_nonblocking(false).expect("blocking");
        let (stream, _) = listener.accept().expect("accept python");
        let err = super::authorize_helper_client(stream.as_raw_fd())
            .expect_err("python3 must not satisfy identifier ternvale");
        tracing::info!(target: "ternvale::sandbox", error = %err, "unsigned/wrong-identity helper client rejected");
        let _ = child.kill();
        let _ = child.wait();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
