//! VM configuration and the shared [`TernvaleError`] for Ternvale.
//!
//! The on-disk format is TOML, parsed with serde. [`VmConfig::from_toml`] rejects
//! unknown fields and runs validation before returning.

mod error;
mod vm;

pub use error::{ConfigError, TernvaleError};
pub use vm::{Disk, Nic, VmConfig};

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
    fn config_error_converts_to_ternvale_error() {
        let error = TernvaleError::from(ConfigError::InvalidCpus { cpus: 0 });
        assert!(error.to_string().contains("cpus"), "{error}");
        let wrapped = TernvaleError::subsystem("hv", "HV_UNSUPPORTED");
        assert_eq!(wrapped.to_string(), "hv: HV_UNSUPPORTED");
    }
}
