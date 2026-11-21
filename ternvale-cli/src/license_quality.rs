//! Degraded-mode quality: export still works; start is refused.

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::ExitCode;

    use crate::api_library;
    use crate::export_cmd;
    use crate::license_gate::TEST_FORCE_ENV;
    use crate::paths::{RUN_DIR_ENV, VMS_DIR_ENV};

    fn scratch() -> PathBuf {
        let dir = std::env::temp_dir().join(format!("tvlk-ql-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("dir");
        dir
    }

    fn write_vm(root: &Path, name: &str) {
        let vm = root.join(name);
        std::fs::create_dir_all(vm.join("disks")).expect("disks");
        let kernel = vm.join("Image");
        std::fs::write(&kernel, b"ARM64kernel").expect("kernel");
        let disk = vm.join("disks").join("disk0.img");
        std::fs::write(&disk, vec![0u8; 64 * 1024]).expect("disk");
        let text = format!(
            r#"
name = "{name}"
cpus = 1
ram_mib = 256
kernel = "{kernel}"
serial_log = "{serial}"

[[disks]]
path = "{disk}"
read_only = false
"#,
            kernel = kernel.display(),
            serial = vm.join("serial.log").display(),
            disk = disk.display(),
        );
        std::fs::write(vm.join("config.toml"), text).expect("cfg");
    }

    #[test]
    fn expired_export_succeeds_start_refused() {
        let dir = scratch();
        // SAFETY: this test owns the library dirs; run with --test-threads=1.
        unsafe {
            std::env::set_var(VMS_DIR_ENV, dir.join("vms"));
            std::env::set_var(RUN_DIR_ENV, dir.join("run"));
            std::env::set_var(TEST_FORCE_ENV, "degraded");
        }
        std::fs::create_dir_all(dir.join("run")).expect("run");
        write_vm(&dir.join("vms"), "kept");
        let bundle = dir.join("kept.tvm");
        assert_eq!(
            export_cmd::export_vm("kept", &bundle, false, false).expect("export"),
            ExitCode::SUCCESS
        );
        assert!(
            bundle.join(crate::tvm::MANIFEST).is_file(),
            "export must write {}",
            crate::tvm::MANIFEST
        );
        let err = api_library::start_vm("kept").expect_err("start refused");
        let msg = format!("{err:#}");
        assert!(
            msg.contains("open, export, and backup remain available"),
            "{msg}"
        );
        unsafe {
            std::env::remove_var(TEST_FORCE_ENV);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }
}
