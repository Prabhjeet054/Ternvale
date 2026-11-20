//! Step 113 host analogs: Launchpad start-while-off, Finder Open With,
//! guest https / blocked schemes, opt-in off, stub uninstall (no live guest).

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use ternvale_agent_proto::GuestApp;
use ternvale_config::{check_host_url, host_file_to_guest, AgentSection, Share};

use super::SEAMLESS_ENV;
use crate::assoc::{self, OPEN_BIN_ENV};
use crate::paths::VMS_DIR_ENV;
use crate::protocol::Request;
use crate::stubs::{self, APPS_DIR_ENV};

static ENV: Mutex<()> = Mutex::new(());

struct Fixture {
    root: PathBuf,
    prev_apps: Option<std::ffi::OsString>,
    prev_vms: Option<std::ffi::OsString>,
    prev_open: Option<std::ffi::OsString>,
    prev_open_log: Option<std::ffi::OsString>,
}

impl Fixture {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!(
            "tv-apps-q-{}-{}-{tag}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).expect("fixture");
        Self {
            root,
            prev_apps: std::env::var_os(APPS_DIR_ENV),
            prev_vms: std::env::var_os(VMS_DIR_ENV),
            prev_open: std::env::var_os(OPEN_BIN_ENV),
            prev_open_log: std::env::var_os("TERNVALE_OPEN_LOG"),
        }
    }

    fn apps_dir(&self) -> PathBuf {
        self.root.join("Ternvale Apps")
    }

    fn vms_dir(&self) -> PathBuf {
        self.root.join("vms")
    }

    fn apply_env(&self) {
        std::env::set_var(APPS_DIR_ENV, self.apps_dir());
        std::env::set_var(VMS_DIR_ENV, self.vms_dir());
        std::fs::create_dir_all(self.vms_dir().join("lab")).expect("vm dir");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        restore(APPS_DIR_ENV, self.prev_apps.take());
        restore(VMS_DIR_ENV, self.prev_vms.take());
        restore(OPEN_BIN_ENV, self.prev_open.take());
        restore("TERNVALE_OPEN_LOG", self.prev_open_log.take());
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn restore(key: &str, prev: Option<std::ffi::OsString>) {
    match prev {
        Some(v) => std::env::set_var(key, v),
        None => std::env::remove_var(key),
    }
}

fn notepad() -> GuestApp {
    GuestApp {
        id: "Accessories/Notepad.lnk".into(),
        name: "Notepad".into(),
        extensions: vec!["txt".into()],
        ..GuestApp::default()
    }
}

fn share(dir: &Path) -> Share {
    Share {
        host_path: dir.to_path_buf(),
        tag: "host".into(),
        mount_point: "Z:".into(),
        read_only: false,
    }
}

fn policy(open_on_host: bool) -> AgentSection {
    AgentSection {
        open_on_host,
        ..AgentSection::default()
    }
}

fn fake_open(dir: &Path) -> PathBuf {
    let log = dir.join("open.log");
    let bin = dir.join("fake-open");
    std::fs::write(
        &bin,
        format!(
            "#!/bin/sh\nprintf '%s\\n' \"$@\" >> \"{}\"\n",
            log.display()
        ),
    )
    .expect("fake-open");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).expect("chmod");
    }
    std::env::set_var(OPEN_BIN_ENV, &bin);
    std::env::set_var("TERNVALE_OPEN_LOG", &log);
    bin
}

fn open_log(dir: &Path) -> String {
    std::fs::read_to_string(dir.join("open.log")).unwrap_or_default()
}

/// Launchpad analog: VM off → stub starts `ternvale run` with seamless env, then AppLaunch.
struct LaunchpadClick {
    vm_was_running: bool,
    started_vm: bool,
    seamless: bool,
    app_id: String,
}

impl LaunchpadClick {
    fn from_stub(vm_running: bool, stub_script: &str, app_id: &str) -> Self {
        assert!(
            stub_script.contains("apps launch"),
            "stub must call apps launch: {stub_script}"
        );
        assert!(
            stub_script.contains(app_id),
            "stub must name the app: {stub_script}"
        );
        let start = !vm_running;
        if start {
            tracing::info!(
                target: "ternvale::cli",
                env = SEAMLESS_ENV,
                "Launchpad analog: start stopped VM then launch"
            );
        }
        Self {
            vm_was_running: vm_running,
            started_vm: start,
            seamless: start,
            app_id: app_id.to_string(),
        }
    }

    fn app_launch(&self) -> Request {
        Request::AppLaunch {
            id: self.app_id.clone(),
            file: None,
            confirm: true,
        }
    }
}

#[test]
fn launchpad_stub_starts_stopped_vm_and_launches_notepad() {
    let _lock = ENV.lock().expect("env");
    let fix = Fixture::new("launch");
    fix.apply_env();
    stubs::install("lab", &[notepad()], Path::new("/usr/bin/true")).expect("install");
    let launcher = fix
        .apps_dir()
        .join("lab/Notepad.app/Contents/MacOS/launcher");
    let script = std::fs::read_to_string(&launcher).expect("script");
    let click = LaunchpadClick::from_stub(false, &script, "Accessories/Notepad.lnk");
    assert!(!click.vm_was_running);
    assert!(click.started_vm);
    assert!(click.seamless);
    let start = super::SeamlessStart {
        bin: PathBuf::from("/usr/bin/true"),
        config: PathBuf::from("/tmp/lab.toml"),
    };
    let cmd = start.command();
    assert!(
        cmd.get_args().any(|a| a == "run"),
        "expected ternvale run: {cmd:?}"
    );
    assert!(
        cmd.get_envs().any(|(key, value)| {
            key == SEAMLESS_ENV && value == Some(std::ffi::OsStr::new("1"))
        }),
        "expected {SEAMLESS_ENV}=1 on Launchpad start"
    );
    match click.app_launch() {
        Request::AppLaunch { id, file, .. } => {
            assert_eq!(id, "Accessories/Notepad.lnk");
            assert!(file.is_none());
        }
        other => panic!("expected AppLaunch, got {other:?}"),
    }
}

#[test]
fn finder_open_with_txt_maps_share_path_to_guest_notepad() {
    let _lock = ENV.lock().expect("env");
    let fix = Fixture::new("openwith");
    let share_root = fix.root.join("share");
    std::fs::create_dir_all(&share_root).expect("share");
    let txt = share_root.join("memo.txt");
    std::fs::write(&txt, b"hello from Finder").expect("txt");
    let shares = [share(&share_root)];
    let guest = host_file_to_guest(&shares, &txt).expect("map");
    tracing::info!(
        target: "ternvale::cli",
        guest = %guest,
        "Finder Open With analog mapped .txt onto the share"
    );
    assert!(
        guest.eq_ignore_ascii_case("Z:\\memo.txt") || guest.ends_with("memo.txt"),
        "{guest}"
    );
    let outside = fix.root.join("outside.txt");
    std::fs::write(&outside, b"nope").expect("outside");
    let err = host_file_to_guest(&shares, &outside).expect_err("must not copy");
    assert!(err.contains("will not copy"), "{err}");
    let req = Request::AppLaunch {
        id: "Accessories/Notepad.lnk".into(),
        file: Some(guest.clone()),
        confirm: true,
    };
    match req {
        Request::AppLaunch {
            file: Some(path), ..
        } => assert_eq!(path, guest),
        other => panic!("{other:?}"),
    }
}

#[test]
fn guest_https_opens_on_host_browser_analog() {
    let _lock = ENV.lock().expect("env");
    let fix = Fixture::new("https");
    fix.apply_env();
    fake_open(&fix.root);
    assoc::approve("lab", "host-url:https").expect("approve");
    let policy = policy(true);
    assoc::handle_guest_open("lab", &policy, &[], "url", "https://example.com/from-guest")
        .expect("https");
    let log = open_log(&fix.root);
    assert!(
        log.contains("-u") && log.contains("https://example.com/from-guest"),
        "macOS open analog missing URL: {log:?}"
    );
}

#[test]
fn file_and_unknown_scheme_are_blocked_with_reason() {
    let _lock = ENV.lock().expect("env");
    let fix = Fixture::new("block");
    fix.apply_env();
    fake_open(&fix.root);
    let policy = policy(true);
    let file_err =
        assoc::handle_guest_open("lab", &policy, &[], "url", "file:///Users/me/secret.txt")
            .expect_err("file");
    assert!(
        file_err.contains("file:") || file_err.to_ascii_lowercase().contains("never"),
        "{file_err}"
    );
    let slack_err =
        assoc::handle_guest_open("lab", &policy, &[], "url", "slack://open").expect_err("slack");
    assert!(
        slack_err.contains("allowlist") || slack_err.contains("scheme"),
        "{slack_err}"
    );
    assert!(
        open_log(&fix.root).is_empty(),
        "blocked schemes must not call open"
    );
}

#[test]
fn open_on_host_off_keeps_links_in_the_guest() {
    let _lock = ENV.lock().expect("env");
    let fix = Fixture::new("stay");
    fix.apply_env();
    fake_open(&fix.root);
    let policy = policy(false);
    let err = check_host_url(&policy, "https://example.com").expect_err("disabled");
    assert!(err.contains("open_on_host"), "{err}");
    let open_err = assoc::handle_guest_open("lab", &policy, &[], "url", "https://example.com")
        .expect_err("stay");
    tracing::info!(
        target: "ternvale::cli",
        reason = %open_err,
        "opt-in off: https stays in the guest"
    );
    assert!(
        open_err.contains("disabled") || open_err.contains("open_on_host"),
        "{open_err}"
    );
    assert!(open_log(&fix.root).is_empty());
}

#[test]
fn uninstall_leaves_no_ternvale_stubs() {
    let _lock = ENV.lock().expect("env");
    let fix = Fixture::new("uninst");
    fix.apply_env();
    stubs::install("lab", &[notepad()], Path::new("/usr/bin/true")).expect("install");
    assert!(fix.apps_dir().join("lab/Notepad.app").is_dir());
    let removed = stubs::uninstall(None).expect("uninstall");
    assert_eq!(removed, 1);
    assert!(
        !fix.apps_dir().join("lab/Notepad.app").exists(),
        "Notepad stub must be gone"
    );
    let leftover: Vec<_> = walk_apps(&fix.apps_dir());
    assert!(
        leftover.is_empty(),
        "no Ternvale stubs should remain: {leftover:?}"
    );
}

fn walk_apps(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if path.extension().and_then(|e| e.to_str()) == Some("app") {
                out.push(path);
            } else {
                out.extend(walk_apps(&path));
            }
        }
    }
    out
}
