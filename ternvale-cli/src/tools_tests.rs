use crate::cli::{Cli, Command};
use crate::tools::{auto_attach_windows_tools, ToolsCommand, TOOLS_ISO_ENV};
use crate::tools_build::{build_test_iso, iso_list};
use clap::Parser;

#[test]
fn install_script_sets_up_winfsp_and_virtiofs() {
    let ps1 = include_str!(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../scripts/windows/Install-TernvaleTools.ps1"
    ));
    assert!(ps1.contains("winfsp.msi"), "{ps1}");
    assert!(ps1.contains("VirtioFsSvc"), "{ps1}");
    assert!(ps1.contains("virtiofs.exe"), "{ps1}");
}

#[test]
fn clap_parses_tools_build_iso() {
    let command = Cli::try_parse_from([
        "ternvale",
        "tools",
        "build-iso",
        "--virtio-win",
        "/tmp/virtio-win",
        "--agent",
        "/tmp/ternvale-agent.exe",
        "--out",
        "/tmp/ternvale-tools.iso",
        "--fetch",
    ])
    .expect("parse")
    .command;
    assert_eq!(
        command,
        Command::Tools {
            action: ToolsCommand::BuildIso {
                virtio_win: Some("/tmp/virtio-win".into()),
                fetch: true,
                agent: Some("/tmp/ternvale-agent.exe".into()),
                out: Some("/tmp/ternvale-tools.iso".into()),
                agent_version: None,
            }
        }
    );
    assert_eq!(
        Cli::try_parse_from([
            "ternvale",
            "tools",
            "outdated",
            "--guest-agent",
            "ternvale-agent 0.1.0",
            "--iso",
            "/tmp/ternvale-tools.iso",
        ])
        .expect("parse")
        .command,
        Command::Tools {
            action: ToolsCommand::Outdated {
                iso: Some("/tmp/ternvale-tools.iso".into()),
                guest_agent: "ternvale-agent 0.1.0".into(),
            }
        }
    );
}

#[test]
fn auto_attach_fills_iso_tools_for_windows_when_the_file_exists() {
    let dir = std::env::temp_dir().join(format!("ternvale-tools-attach-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let iso = dir.join("ternvale-tools.iso");
    let mut bytes = vec![0u8; 17 * 2048];
    bytes[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(b"CD001");
    std::fs::write(&iso, bytes).expect("iso");
    std::env::set_var(TOOLS_ISO_ENV, &iso);
    let text = "name = \"w\"\nprofile = \"windows\"\ncpus = 2\nram_mib = 4096\nfirmware = \"/fw.fd\"\nserial_log = \"/tmp/s.log\"\n[[disks]]\npath = \"/tmp/d.img\"\nread_only = false\n";
    let mut config = ternvale_config::VmConfig::parse(text).expect("parse");
    auto_attach_windows_tools(&mut config);
    std::env::remove_var(TOOLS_ISO_ENV);
    let tools = config.iso.as_ref().and_then(|iso| iso.tools.clone());
    assert_eq!(tools.as_deref(), Some(iso.as_path()));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn build_iso_contains_install_script_agent_and_manifest() {
    let dir = std::env::temp_dir().join(format!("ternvale-tools-iso-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let out = dir.join("ternvale-tools.iso");
    let built = match build_test_iso(&out, &dir) {
        Ok(built) => built,
        Err(error) => {
            let _ = std::fs::remove_dir_all(&dir);
            panic!("{error:#}");
        }
    };
    assert!(out.is_file(), "iso missing");
    assert!(built.sidecar.is_file(), "sidecar missing");
    let listing = iso_list(&out).expect("tar -tf").to_ascii_lowercase();
    for name in [
        "manifest.json",
        "install.ps1",
        "ternvale-agent.exe",
        "viostor.inf",
    ] {
        assert!(
            listing.contains(name),
            "missing {name} in listing:\n{listing}"
        );
    }
    let json = std::fs::read_to_string(&built.sidecar).expect("sidecar");
    let parsed = crate::tools_manifest::ToolsManifest::parse(&json).expect("manifest");
    assert_eq!(parsed.version, crate::tools_manifest::VERSION);
    assert!(!parsed.guest_outdated(&parsed.agent_version));
    crate::tools_verify::verify_tree(&parsed, &dir).ok();
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bumped_iso_agent_version_flags_the_guest_as_outdated() {
    let man = crate::tools_manifest::ToolsManifest::new(
        "0.1.1".into(),
        "0.1.1".into(),
        "0.1.302".into(),
        Vec::new(),
    );
    assert!(man.guest_outdated("ternvale-agent 0.1.0"));
    let mut status = crate::protocol::StatusJson {
        name: "w".into(),
        state: "running".into(),
        cpus: 1,
        uptime_ms: 0,
        state_ms: 0,
        paused_ms: 0,
        pauses: 0,
        stop_cause: None,
        failure: None,
        agent: Some(crate::protocol::AgentJson {
            state: "connected".into(),
            version: Some(3),
            os: Some("Windows".into()),
            agent: Some("ternvale-agent 0.1.0".into()),
            connected_ms: Some(1),
            last_pong_ms: None,
            rtt_us: None,
            connects: 1,
            disconnects: 0,
            pings: 0,
            pongs: 0,
            last_error: None,
        }),
        tools_outdated: None,
        tools_iso_agent: None,
        backend: None,
        capabilities: None,
    };
    let dir = std::env::temp_dir().join(format!("ternvale-tools-outdated-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let iso = dir.join("ternvale-tools.iso");
    std::fs::write(&iso, b"not-an-iso").expect("iso stub");
    let sidecar = crate::tools_build::sidecar_path(&iso);
    std::fs::write(&sidecar, serde_json::to_vec(&man).expect("json")).expect("sidecar");
    crate::tools::fill_tools_outdated(&mut status, Some(&iso));
    assert_eq!(status.tools_outdated, Some(true));
    assert_eq!(status.tools_iso_agent.as_deref(), Some("0.1.1"));
    let rendered = crate::commands::render(&status, None);
    assert!(rendered.contains("tools: outdated"), "{rendered}");
    let _ = std::fs::remove_dir_all(&dir);
}
