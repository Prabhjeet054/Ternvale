//! Linked-clone E2E: three concurrent boots on shared-base stacks, per-clone writes,
//! unchanged base digest, delete two clones, verify the survivor, reject writable base open.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use ternvale_config::{Disk, VmConfig};
use ternvale_devices::try_open_disk_for_write;
use ternvale_devices::{attach_disks, last_lines, Pl011, Step};
use ternvale_snapshot::{
    acquire_base_ref, base_ref_count, create_linked_stack, release_disk_base, shared_base_path,
};
use ternvale_vmm::{ExitReason, Machine};

use crate::common::{
    assets_root, banner_timeout, drive, init_logging, log_dir, restore_stdin, stdin_pipe,
    write_result,
};

const CLONES: [&str; 3] = ["a", "b", "c"];
const SOURCE_HOLDER: &str = "clone-source";
const DONE: &str = "ternvale clone ";
const VERIFY_OK: &str = "ternvale clone verify ";

#[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all)]
pub fn run() -> Result<(), String> {
    let virtio = assets_root().join("virtio-root");
    let clone_assets = assets_root().join("clone-boot");
    for path in [
        virtio.join("Image"),
        virtio.join("rootfs.ext4"),
        clone_assets.join("initramfs.cpio"),
    ] {
        if !path.is_file() {
            return Err(format!(
                "missing {} (run ./scripts/make-rootfs.sh and ./scripts/make-clone-initramfs.sh)",
                path.display()
            ));
        }
    }

    let log_dir = log_dir();
    let guard = init_logging("boot-clone", &log_dir)?;
    tracing::info!(
        target: "ternvale::boot",
        dir = %log_dir.display(),
        host_log = %guard.log_path().display(),
        scenario = "clone",
        "boot harness logs"
    );

    let work = log_dir.join("clone-work");
    std::fs::create_dir_all(&work).map_err(|e| format!("work dir: {e}"))?;
    let refs_dir = work.join("refs");
    std::fs::create_dir_all(&refs_dir).map_err(|e| format!("refs dir: {e}"))?;
    // SAFETY: boot harness runs in a dedicated process; tests set the same vars.
    unsafe {
        std::env::set_var("TERNVALE_REFS_DIR", &refs_dir);
    }

    let stacks = if std::env::var_os("TERNVALE_VMS_DIR").is_some() {
        stacks_from_library_vms(&work)?
    } else {
        stacks_from_linked_setup(&work, &virtio)?
    };

    let base = shared_base_path(&stacks[0].stack).map_err(|e| e.to_string())?;
    let digest_before = digest_file(&base)?;
    tracing::info!(
        target: "ternvale::boot",
        base = %base.display(),
        digest = %digest_before,
        "shared base digest before concurrent boots"
    );

    let kernel = virtio.join("Image");
    let initrd = clone_assets.join("initramfs.cpio");
    let host_log = guard.log_path().to_path_buf();
    let started = Instant::now();

    let mut handles = Vec::new();
    for entry in &stacks {
        let kernel = kernel.clone();
        let initrd = initrd.clone();
        let host_log = host_log.clone();
        let log_dir = log_dir.clone();
        let stack = entry.stack.clone();
        let id = entry.id.clone();
        let holder = entry.holder.clone();
        handles.push(thread::spawn(move || {
            boot_once(
                &kernel, &initrd, &stack, &id, &holder, false, &log_dir, &host_log,
            )
        }));
    }
    for (entry, handle) in stacks.iter().zip(handles) {
        handle
            .join()
            .map_err(|_| format!("boot thread for {} panicked", entry.id))??;
    }

    tracing::info!(
        target: "ternvale::boot",
        concurrent_boot_ms = started.elapsed().as_millis() as u64,
        "three clone boots finished"
    );

    let digest_after = digest_file(&base)?;
    if digest_before != digest_after {
        return Err(format!(
            "shared base digest changed after boots (before {digest_before}, after {digest_after})"
        ));
    }

    for entry in &stacks {
        let marker = marker_bytes(&entry.id);
        let delta = entry.delta.clone();
        if !file_contains(&delta, &marker) {
            return Err(format!(
                "delta {} missing marker for clone {}",
                delta.display(),
                entry.id
            ));
        }
        for other in CLONES {
            if other == entry.id {
                continue;
            }
            if file_contains(&delta, &marker_bytes(other)) {
                return Err(format!(
                    "delta {} leaked marker from clone {}",
                    delta.display(),
                    other
                ));
            }
        }
    }

    for entry in stacks.iter().take(2) {
        delete_clone_vm(entry)?;
    }

    let remaining = &stacks[2];
    if !base.is_file() {
        return Err(format!("shared base missing: {}", base.display()));
    }
    let count = base_ref_count(&base).map_err(|e| e.to_string())?;
    tracing::info!(
        target: "ternvale::snapshot",
        base = %base.display(),
        count,
        "refcount after deleting two clones"
    );
    if count == 0 {
        return Err("expected refcount > 0 while survivor holds the base".into());
    }

    boot_once(
        &kernel,
        &initrd,
        &remaining.stack,
        &remaining.id,
        &remaining.holder,
        true,
        &log_dir,
        guard.log_path(),
    )?;

    let Err(err) = try_open_disk_for_write(&base) else {
        return Err("writable open of shared base should be rejected".into());
    };
    let detail = format!("{err:#}");
    if !detail.contains("shared clone base") {
        return Err(format!("unexpected open error: {detail}"));
    }
    tracing::info!(
        target: "ternvale::virtio::blk",
        base = %base.display(),
        "host rejected writable open of shared base"
    );

    tracing::info!(
        target: "ternvale::boot",
        scenario = "clone",
        "linked clone harness passed"
    );
    drop(guard);
    Ok(())
}

struct CloneStack {
    id: String,
    holder: String,
    vm_name: String,
    stack: PathBuf,
    delta: PathBuf,
}

fn stacks_from_linked_setup(work: &Path, virtio: &Path) -> Result<Vec<CloneStack>, String> {
    let base = work.join("base.img");
    std::fs::copy(virtio.join("rootfs.ext4"), &base)
        .map_err(|e| format!("copy base rootfs: {e}"))?;
    acquire_base_ref(&base, SOURCE_HOLDER).map_err(|e| e.to_string())?;

    let mut out = Vec::new();
    for id in CLONES {
        let holder = format!("clone-{id}");
        let vm_name = format!("clone-{id}");
        let dest = work.join(&vm_name).join("disks");
        let stack = create_linked_stack(&base, &dest, "disk0.tstack", "disk0.tdelta", &holder)
            .map_err(|e| e.to_string())?;
        let delta = dest.join("disk0.tdelta");
        out.push(CloneStack {
            id: id.to_string(),
            holder,
            vm_name,
            stack,
            delta,
        });
    }
    Ok(out)
}

fn stacks_from_library_vms(work: &Path) -> Result<Vec<CloneStack>, String> {
    let names = std::env::var("TERNVALE_CLONE_VMS").map_err(|_| {
        "TERNVALE_VMS_DIR is set but TERNVALE_CLONE_VMS is missing (comma-separated VM names)"
            .to_string()
    })?;
    let vms_root =
        std::env::var("TERNVALE_VMS_DIR").map_err(|_| "TERNVALE_VMS_DIR unset".to_string())?;
    let mut out = Vec::new();
    for vm_name in names.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let config_path = PathBuf::from(&vms_root).join(vm_name).join("config.toml");
        let config = VmConfig::from_path(&config_path)
            .map_err(|e| format!("load {}: {e:#}", config_path.display()))?;
        let disk = config
            .disks
            .first()
            .ok_or_else(|| format!("VM {vm_name} has no disks"))?
            .path
            .clone();
        let id = vm_name
            .strip_prefix("clone-")
            .unwrap_or(vm_name)
            .to_string();
        let delta = disk.parent().unwrap_or(Path::new(".")).join("disk0.tdelta");
        out.push(CloneStack {
            id,
            holder: vm_name.to_string(),
            vm_name: vm_name.to_string(),
            stack: disk,
            delta,
        });
    }
    if out.len() != CLONES.len() {
        return Err(format!(
            "expected {} clone VMs, got {}",
            CLONES.len(),
            out.len()
        ));
    }
    let _ = work;
    Ok(out)
}

fn delete_clone_vm(entry: &CloneStack) -> Result<(), String> {
    if std::env::var_os("TERNVALE_CLONE_USE_CLI").is_some() {
        let bin = std::env::var("TERNVALE_BIN").unwrap_or_else(|_| {
            PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default())
                .join("../../target/debug/ternvale")
                .to_string_lossy()
                .into_owned()
        });
        tracing::info!(
            target: "ternvale::cli",
            vm = entry.vm_name,
            bin = %bin,
            "deleting clone via CLI"
        );
        let status = Command::new(&bin)
            .args(["vm", "delete", &entry.vm_name])
            .status()
            .map_err(|e| format!("spawn {bin}: {e}"))?;
        if !status.success() {
            return Err(format!(
                "ternvale vm delete {} failed: {status}",
                entry.vm_name
            ));
        }
        Ok(())
    } else {
        release_disk_base(&entry.stack, &entry.holder).map_err(|e| e.to_string())?;
        tracing::info!(
            target: "ternvale::snapshot",
            vm = %entry.vm_name,
            holder = %entry.holder,
            "released clone disk base refcount"
        );
        Ok(())
    }
}

fn boot_once(
    kernel: &Path,
    initrd: &Path,
    stack: &Path,
    id: &str,
    holder: &str,
    verify: bool,
    log_dir: &Path,
    host_log: &Path,
) -> Result<(), String> {
    let label = if verify {
        format!("verify-{id}")
    } else {
        format!("boot-{id}")
    };
    let serial_log = log_dir.join(format!("{label}-serial.log"));
    let cmdline = if verify {
        format!(
            "console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init ternvale.clone_id={id} ternvale.clone_verify=1"
        )
    } else {
        format!("console=ttyAMA0 earlycon=pl011,0x9000000 rdinit=/init ternvale.clone_id={id}")
    };
    let expect = if verify {
        format!("{VERIFY_OK}{id} ok")
    } else {
        format!("{DONE}{id} done")
    };
    let steps = vec![
        Step::Expect {
            pattern: "Linux version".to_string(),
            timeout: banner_timeout(),
        },
        Step::Expect {
            pattern: expect,
            timeout: Duration::from_secs(120),
        },
    ];

    let vm = VmConfig {
        name: format!("boot-clone-{holder}-{label}"),
        profile: Default::default(),
        iso: None,
        cpus: 1,
        ram_mib: 256,
        kernel: kernel.to_path_buf(),
        initrd: Some(initrd.to_path_buf()),
        cmdline,
        boot_disk: false,
        disks: vec![Disk {
            path: stack.to_path_buf(),
            read_only: false,
        }],
        nics: Vec::new(),
        serial_log: serial_log.clone(),
        firmware: None,
        nvram: None,
        firmware_tables: None,
        vsock: None,
        tpm: None,
        usb: None,
        audio: None,
    };
    tracing::info!(
        target: "ternvale::boot",
        id,
        verify,
        stack = %stack.display(),
        "starting clone boot"
    );
    let uart = Pl011::open(&serial_log).map_err(|e| e.to_string())?;
    let cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (mut input, saved) = stdin_pipe()?;
    let flag = Arc::clone(&cancel);
    let serial_for_feed = serial_log.clone();
    let host = host_log.to_path_buf();
    let feeder = thread::spawn(move || drive(&serial_for_feed, &mut input, &flag, &host, steps));
    let disk = stack.to_path_buf();
    let exit = Machine::run_with(&vm, Box::new(uart), Arc::clone(&cancel), move |attach| {
        attach_disks(attach, &[(disk, false)])
    });
    cancel.store(true, Ordering::Release);
    let script_result = feeder
        .join()
        .unwrap_or_else(|_| Err("feeder panicked".to_string()));
    restore_stdin(saved);
    let serial = std::fs::read_to_string(&serial_log).unwrap_or_default();
    write_result(log_dir, &exit, &script_result, &serial);
    script_result?;
    match exit {
        Ok(ExitReason::SystemOff) => Ok(()),
        other => Err(format!(
            "{label} exit={other:?}\nlog={}\n--- serial ---\n{}",
            host_log.display(),
            last_lines(&serial, 40)
        )),
    }
}

fn marker_bytes(id: &str) -> Vec<u8> {
    format!("TERNVALE_CLONE_FILE_{id}\n").into_bytes()
}

fn file_contains(path: &Path, needle: &[u8]) -> bool {
    std::fs::read(path)
        .map(|bytes| bytes.windows(needle.len()).any(|w| w == needle))
        .unwrap_or(false)
}

fn digest_file(path: &Path) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let mut file =
        std::fs::File::open(path).map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 65536];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| format!("read {}: {e}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}
