use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

use crate::create_cmd::{create_windows, CreateWindows};
use crate::unattend_iso::require_tools_iso;

fn disc(path: &std::path::Path) {
    let mut bytes = vec![0u8; 17 * 2048];
    bytes[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(b"CD001");
    fs::write(path, bytes).expect("iso");
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("tv-create-{}-{n}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("scratch");
        Self(dir)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn missing_tools_iso_is_a_clear_error() {
    let scratch = Scratch::new();
    let gone = scratch.0.join("missing.iso");
    let error = require_tools_iso(&gone).expect_err("missing");
    let text = format!("{error:#}");
    assert!(text.contains("tools ISO missing"), "{text}");
    assert!(
        text.contains("hang") || text.contains("build-iso"),
        "{text}"
    );
}

#[test]
fn non_disc_tools_path_is_a_clear_error() {
    let scratch = Scratch::new();
    let raw = scratch.0.join("not.iso");
    fs::write(&raw, b"hello").expect("raw");
    let error = require_tools_iso(&raw).expect_err("not a disc");
    let text = format!("{error:#}");
    assert!(
        text.contains("not an ISO") || text.contains("hang"),
        "{text}"
    );
}

#[test]
fn unattended_create_writes_config_without_the_password() {
    let scratch = Scratch::new();
    let installer = scratch.0.join("win.iso");
    disc(&installer);
    let tools_tree = scratch.0.join("virtio-tree");
    fs::create_dir_all(tools_tree.join("viostor/w11/ARM64")).expect("viostor");
    fs::write(tools_tree.join("viostor/w11/ARM64/viostor.inf"), b"; inf\n").expect("inf");
    fs::create_dir_all(tools_tree.join("NetKVM/w11/ARM64")).expect("netkvm");
    fs::write(tools_tree.join("NetKVM/w11/ARM64/netkvm.inf"), b"; inf\n").expect("inf");
    let tools = scratch.0.join("tools.iso");
    crate::tools_hybrid::make_hybrid_iso(&tools_tree, &tools).expect("tools iso");
    let firmware = scratch.0.join("QEMU_EFI.fd");
    fs::write(&firmware, b"fw").expect("fw");
    let password_file = scratch.0.join("pw.txt");
    fs::write(&password_file, "TestP@ssw0rd!\n").expect("pw");
    let vms = scratch.0.join("vms");
    fs::create_dir_all(&vms).expect("vms");
    std::env::set_var(crate::paths::VMS_DIR_ENV, &vms);
    std::env::set_var(crate::paths::RUN_DIR_ENV, scratch.0.join("run"));
    std::env::set_var("TERNVALE_SKIP_KEYCHAIN", "1");
    let name = format!("w{}", std::process::id());
    create_windows(CreateWindows {
        name: name.clone(),
        iso: installer,
        unattended: true,
        edition: "Windows 11 Pro".into(),
        disk_size: "64M".into(),
        firmware: Some(firmware),
        tools_iso: Some(tools),
        user: "Ternvale".into(),
        password_env: None,
        password_file: Some(password_file),
        product_key_env: None,
        product_key_file: None,
        wait: false,
        timeout_secs: 1,
    })
    .expect("create");
    let config = fs::read_to_string(vms.join(&name).join("config.toml")).expect("config");
    assert!(!config.contains("TestP@ssw0rd!"), "{config}");
    assert!(config.contains("profile = \"windows\""));
    assert!(config.contains("unattend"));
    let xml = {
        // Autounattend lives only inside the ISO; grep the ISO bytes.
        fs::read(vms.join(&name).join("unattend.iso")).expect("iso")
    };
    let blob = String::from_utf8_lossy(&xml);
    assert!(!blob.contains("TestP@ssw0rd!"), "plaintext on unattend ISO");
    std::env::remove_var(crate::paths::VMS_DIR_ENV);
    std::env::remove_var(crate::paths::RUN_DIR_ENV);
    std::env::remove_var("TERNVALE_SKIP_KEYCHAIN");
}

#[test]
fn create_without_unattended_is_rejected() {
    let error = create_windows(CreateWindows {
        name: "x".into(),
        iso: "/nope".into(),
        unattended: false,
        edition: "Windows 11 Pro".into(),
        disk_size: "64G".into(),
        firmware: None,
        tools_iso: None,
        user: "Ternvale".into(),
        password_env: None,
        password_file: None,
        product_key_env: None,
        product_key_file: None,
        wait: false,
        timeout_secs: 1,
    })
    .expect_err("need unattended");
    assert!(format!("{error:#}").contains("--unattended"));
}
