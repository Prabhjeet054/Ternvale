use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use ternvale_devices::VirtioIrq;
use ternvale_gpu::VirtioGpu;
use ternvale_vmm::{GuestMemory, HOST_PAGE_SIZE};

use super::*;

/// A short per-test directory under /tmp (`sun_path` is 104 bytes).
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let dir = PathBuf::from(format!(
            "/tmp/tvdisp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        if dir.exists() {
            std::fs::remove_dir_all(&dir).expect("clear leftovers");
        }
        std::fs::create_dir_all(&dir).expect("scratch dir");
        Self(dir)
    }

    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.0.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).expect("write script");
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        path
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            eprintln!("could not remove {}: {error}", self.0.display());
        }
    }
}

fn gpu_display() -> GpuDisplay {
    let mut mem = GuestMemory::new().expect("memory");
    mem.add_region(0x4000_0000, HOST_PAGE_SIZE).expect("region");
    let gpu = VirtioGpu::new(
        GpuConfig::default(),
        Arc::new(Mutex::new(mem)),
        VirtioIrq::new(),
    )
    .expect("gpu");
    gpu.display()
}

#[test]
fn display_mode_names_match_the_flag_values() {
    assert_eq!(DisplayMode::default(), DisplayMode::None);
    assert_eq!(DisplayMode::None.to_string(), "none");
    assert_eq!(DisplayMode::Window.to_string(), "window");
}

#[test]
fn viewer_is_looked_up_beside_the_binary_then_in_target_viewer() {
    let dir = Path::new("/x/target/debug");
    assert_eq!(
        viewer_candidates(dir),
        [
            PathBuf::from("/x/target/debug/ternvale-viewer"),
            PathBuf::from("/x/target/debug/../viewer/ternvale-viewer"),
        ]
    );
    let scratch = Scratch::new();
    let bin = scratch.0.join("debug");
    std::fs::create_dir_all(&bin).expect("bin dir");
    let error = locate_viewer(None, &bin).expect_err("nothing built");
    assert!(format!("{error:#}").contains("make viewer"), "{error:#}");
    std::fs::create_dir_all(scratch.0.join("viewer")).expect("viewer dir");
    let built = scratch.script("viewer/ternvale-viewer", "exit 0");
    let found = locate_viewer(None, &bin).expect("target/viewer");
    assert_eq!(
        found.canonicalize().expect("canon"),
        built.canonicalize().expect("canon")
    );
    let beside = scratch.script("debug/ternvale-viewer", "exit 0");
    assert_eq!(locate_viewer(None, &bin).expect("beside"), beside);
}

#[test]
fn viewer_override_must_be_a_file() {
    let scratch = Scratch::new();
    let error = locate_viewer(Some(scratch.0.join("missing")), &scratch.0).expect_err("missing");
    assert!(format!("{error:#}").contains(VIEWER_ENV), "{error:#}");
    let custom = scratch.script("custom-viewer", "exit 0");
    assert_eq!(
        locate_viewer(Some(custom.clone()), &scratch.0).expect("override"),
        custom
    );
}

#[test]
fn display_socket_sits_beside_the_control_socket() {
    assert_eq!(
        socket_path(Path::new("/tmp/tv/vm-1.sock")).expect("path"),
        PathBuf::from("/tmp/tv/vm-1.display.sock")
    );
    let long = format!("/tmp/{}.sock", "x".repeat(MAX_SOCKET_PATH - 10));
    assert!(long.len() <= MAX_SOCKET_PATH);
    let error = socket_path(Path::new(&long)).expect_err("too long");
    assert!(format!("{error:#}").contains(RUN_DIR_ENV), "{error:#}");
}

fn plan(scratch: &Scratch, viewer: PathBuf) -> DisplayPlan {
    DisplayPlan {
        name: "vm".into(),
        viewer,
        socket: scratch.0.join("vm.display.sock"),
        fps: 30,
    }
}

#[test]
fn session_launches_the_viewer_and_removes_the_socket_on_stop() {
    let _child = crate::tests::CHILD_PROCESS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    let args = scratch.0.join("args");
    let viewer = scratch.script("viewer", &format!("echo \"$@\" > {}", args.display()));
    let plan = plan(&scratch, viewer);
    let session = plan.start(gpu_display()).expect("start");
    assert!(plan.socket.exists(), "presenter listening");
    drop(session);
    assert!(!plan.socket.exists(), "socket removed");
    let seen = std::fs::read_to_string(&args).expect("viewer ran");
    assert_eq!(
        seen.trim(),
        format!("--socket {} --title Ternvale — vm", plan.socket.display())
    );
}

#[test]
fn session_kills_a_viewer_that_does_not_exit() {
    let _child = crate::tests::CHILD_PROCESS
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let scratch = Scratch::new();
    let viewer = scratch.script("stubborn", "trap '' TERM HUP\nexec sleep 30");
    let session = plan(&scratch, viewer).start(gpu_display()).expect("start");
    let started = Instant::now();
    drop(session);
    let waited = started.elapsed();
    assert!(waited >= VIEWER_EXIT_GRACE, "{waited:?}");
    assert!(
        waited < VIEWER_EXIT_GRACE + Duration::from_secs(2),
        "{waited:?}"
    );
}

#[test]
fn session_reports_a_viewer_that_cannot_launch() {
    let scratch = Scratch::new();
    let plan = plan(&scratch, scratch.0.join("not-there"));
    let error = plan.start(gpu_display()).err().expect("launch fails");
    assert!(
        format!("{error:#}").contains("launch the dev viewer"),
        "{error:#}"
    );
    assert!(
        !plan.socket.exists(),
        "presenter dropped with the failed launch"
    );
}
