//! VM configuration and the shared [`TernvaleError`] for Ternvale.
//!
//! The on-disk format is TOML, parsed with serde. [`VmConfig::from_toml`] rejects
//! unknown fields and runs validation before returning.

mod error;
mod vm;
mod vsock;

pub use error::{ConfigError, TernvaleError};
pub use vm::{default_nvram_path, Disk, Nic, VmConfig};
pub use vsock::{VsockSection, MAX_UDS_DIR};

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::{ConfigError, TernvaleError, VmConfig};

    struct Fixture {
        dir: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            use std::sync::atomic::{AtomicU64, Ordering};
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let id = NEXT.fetch_add(1, Ordering::Relaxed);
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|duration| duration.as_nanos())
                .unwrap_or(0);
            let dir = std::env::temp_dir().join(format!(
                "ternvale-config-{}-{}-{}",
                std::process::id(),
                nanos,
                id
            ));
            fs::create_dir_all(&dir).expect("temp dir");
            for name in ["kernel", "initrd", "disk.img", "firmware.fd"] {
                fs::write(dir.join(name), b"x").expect("temp file");
            }
            Self { dir }
        }

        fn path(&self, name: &str) -> PathBuf {
            self.dir.join(name)
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let removed = fs::remove_dir_all(&self.dir);
            if let Err(err) = removed {
                panic!("remove {}: {err}", self.dir.display());
            }
        }
    }

    fn sample(fix: &Fixture) -> String {
        format!(
            r#"
name = "alpine"
cpus = 2
ram_mib = 512
kernel = "{kernel}"
initrd = "{initrd}"
cmdline = "console=ttyAMA0"
serial_log = "{serial}"
firmware = "{firmware}"

[[disks]]
path = "{disk}"
read_only = false

[[nics]]
backend = "user"
"#,
            kernel = toml_path(&fix.path("kernel")),
            initrd = toml_path(&fix.path("initrd")),
            serial = toml_path(&fix.path("serial.log")),
            firmware = toml_path(&fix.path("firmware.fd")),
            disk = toml_path(&fix.path("disk.img")),
        )
    }

    fn toml_path(path: &Path) -> String {
        path.display().to_string().replace('\\', "\\\\")
    }

    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-config");
    }

    #[test]
    fn valid_config_round_trips() {
        let fix = Fixture::new();
        let config = VmConfig::from_toml(&sample(&fix)).expect("parse");
        assert_eq!(config.name, "alpine");
        assert_eq!(config.cpus, 2);
        assert_eq!(config.ram_mib, 512);
        assert_eq!(config.cmdline, "console=ttyAMA0");
        assert!(!config.boot_disk);
        assert_eq!(config.disks.len(), 1);
        assert!(!config.disks[0].read_only);
        assert_eq!(config.nics.len(), 1);
        assert_eq!(config.nics[0].backend, "user");
        let text = config.to_toml().expect("serialize");
        let again = VmConfig::from_toml(&text).expect("reparse");
        assert_eq!(config, again);
    }

    #[test]
    fn rejects_ram_that_is_not_a_multiple_of_16() {
        let fix = Fixture::new();
        let text = sample(&fix).replace("ram_mib = 512", "ram_mib = 24");
        let error = VmConfig::from_toml(&text).expect_err("bad ram");
        assert!(matches!(error, ConfigError::InvalidRam { ram_mib: 24 }));
        assert!(error.to_string().contains("ram_mib"), "{error}");
    }

    #[test]
    fn rejects_zero_cpus() {
        let fix = Fixture::new();
        let text = sample(&fix).replace("cpus = 2", "cpus = 0");
        let error = VmConfig::from_toml(&text).expect_err("zero cpus");
        assert!(matches!(error, ConfigError::InvalidCpus { cpus: 0 }));
        assert!(error.to_string().contains("cpus"), "{error}");
    }

    #[test]
    fn rejects_missing_kernel() {
        let fix = Fixture::new();
        let missing = fix.path("no-such-kernel");
        let text = sample(&fix).replace(&toml_path(&fix.path("kernel")), &toml_path(&missing));
        let error = VmConfig::from_toml(&text).expect_err("missing kernel");
        match &error {
            ConfigError::MissingFile { field, .. } => assert_eq!(field, "kernel"),
            other => panic!("expected missing kernel, got {other}"),
        }
        assert!(error.to_string().contains("kernel"), "{error}");
    }

    #[test]
    fn rejects_unknown_field() {
        let fix = Fixture::new();
        let text = format!("{}\nbogus = true\n", sample(&fix));
        let error = VmConfig::from_toml(&text).expect_err("unknown field");
        assert!(error.to_string().contains("bogus"), "{error}");
    }

    #[test]
    fn rejects_boot_disk_without_disks() {
        let fix = Fixture::new();
        let text = format!(
            r#"
name = "diskboot"
cpus = 1
ram_mib = 256
kernel = "{kernel}"
boot_disk = true
serial_log = "{serial}"
"#,
            kernel = toml_path(&fix.path("kernel")),
            serial = toml_path(&fix.path("serial.log")),
        );
        let error = VmConfig::from_toml(&text).expect_err("boot_disk without disks");
        assert!(matches!(error, ConfigError::BootDiskWithoutDisks));
        assert!(error.to_string().contains("boot_disk"), "{error}");
    }

    #[test]
    fn parse_skips_validation_but_not_syntax() {
        let text = "name = \"x\"\ncpus = 99\nram_mib = 256\nkernel = \"/no/such/Image\"\nserial_log = \"/no/dir/s.log\"\n";
        let config = VmConfig::parse(text).expect("parse without validation");
        assert_eq!(config.cpus, 99);
        assert!(VmConfig::from_toml(text).is_err());
        let error = VmConfig::parse("name = ").expect_err("bad toml");
        assert!(matches!(error, ConfigError::Parse { .. }), "{error}");
    }

    #[test]
    fn accepts_boot_disk_with_a_disk() {
        let fix = Fixture::new();
        let text = format!(
            r#"
name = "diskboot"
cpus = 1
ram_mib = 256
kernel = "{kernel}"
boot_disk = true
serial_log = "{serial}"

[[disks]]
path = "{disk}"
read_only = false
"#,
            kernel = toml_path(&fix.path("kernel")),
            serial = toml_path(&fix.path("serial.log")),
            disk = toml_path(&fix.path("disk.img")),
        );
        let config = VmConfig::from_toml(&text).expect("parse");
        assert!(config.boot_disk);
        assert_eq!(config.disks.len(), 1);
    }

    fn firmware_only(fix: &Fixture, extra: &str) -> String {
        format!(
            r#"
name = "uefi"
cpus = 1
ram_mib = 256
serial_log = "{serial}"
firmware = "{firmware}"
{extra}
"#,
            serial = toml_path(&fix.path("serial.log")),
            firmware = toml_path(&fix.path("firmware.fd")),
        )
    }

    #[test]
    fn firmware_boot_does_not_need_a_kernel() {
        let fix = Fixture::new();
        let config = VmConfig::from_toml(&firmware_only(&fix, "")).expect("parse");
        assert!(config.kernel.as_os_str().is_empty());
        let text = config.to_toml().expect("serialize");
        assert!(!text.contains("kernel"), "{text}");
        assert_eq!(VmConfig::from_toml(&text).expect("reparse"), config);
    }

    #[test]
    fn direct_boot_still_needs_a_kernel() {
        let fix = Fixture::new();
        let text = firmware_only(&fix, "").replace(
            &format!("firmware = \"{}\"", toml_path(&fix.path("firmware.fd"))),
            "",
        );
        let error = VmConfig::from_toml(&text).expect_err("no kernel");
        assert!(
            matches!(&error, ConfigError::MissingFile { field, .. } if field == "kernel"),
            "{error}"
        );
    }

    #[test]
    fn nvram_defaults_per_vm_and_honours_an_explicit_path() {
        let fix = Fixture::new();
        let config = VmConfig::from_toml(&firmware_only(&fix, "")).expect("parse");
        let default = config.nvram_path().expect("path").expect("firmware set");
        assert!(
            default.ends_with("Library/Application Support/Ternvale/uefi/nvram.fd"),
            "{}",
            default.display()
        );
        let explicit = fix.path("vars.fd");
        let text = firmware_only(&fix, &format!("nvram = \"{}\"", toml_path(&explicit)));
        let config = VmConfig::from_toml(&text).expect("parse explicit");
        assert_eq!(config.nvram_path().expect("path"), Some(explicit));
        let direct = VmConfig::from_toml(&sample(&fix).replace(
            &format!("firmware = \"{}\"", toml_path(&fix.path("firmware.fd"))),
            "",
        ))
        .expect("direct");
        assert_eq!(direct.nvram_path().expect("path"), None);
    }

    #[test]
    fn rejects_nvram_in_a_missing_directory() {
        let fix = Fixture::new();
        let bad = fix.path("no-such-dir").join("vars.fd");
        let text = firmware_only(&fix, &format!("nvram = \"{}\"", toml_path(&bad)));
        let error = VmConfig::from_toml(&text).expect_err("bad nvram");
        assert!(matches!(error, ConfigError::NvramPath { .. }), "{error}");
        let text = firmware_only(&fix, &format!("nvram = \"{}\"", toml_path(&fix.dir)));
        let error = VmConfig::from_toml(&text).expect_err("dir nvram");
        assert!(matches!(error, ConfigError::NvramPath { .. }), "{error}");
    }

    #[test]
    fn vsock_section_parses_round_trips_and_is_validated() {
        let fix = Fixture::new();
        let config = VmConfig::from_toml(&sample(&fix)).expect("no vsock");
        assert_eq!(config.vsock, None);
        let text = format!("{}\n[vsock]\ncid = 7\n", sample(&fix));
        let config = VmConfig::from_toml(&text).expect("vsock");
        let vsock = config.vsock.clone().expect("section");
        assert_eq!((vsock.cid, vsock.agent), (Some(7), true));
        let again = VmConfig::from_toml(&config.to_toml().expect("serialize")).expect("reparse");
        assert_eq!(again, config);
        let text = format!("{}\n[vsock]\ncid = 2\n", sample(&fix));
        let error = VmConfig::from_toml(&text).expect_err("reserved cid");
        assert!(
            matches!(
                error,
                ConfigError::InvalidVsock {
                    field: "vsock.cid",
                    ..
                }
            ),
            "{error}"
        );
    }

    #[test]
    fn config_error_converts_to_ternvale_error() {
        let error = TernvaleError::from(ConfigError::InvalidCpus { cpus: 0 });
        assert!(error.to_string().contains("cpus"), "{error}");
        let wrapped = TernvaleError::subsystem("hv", "HV_UNSUPPORTED");
        assert_eq!(wrapped.to_string(), "hv: HV_UNSUPPORTED");
    }
}
