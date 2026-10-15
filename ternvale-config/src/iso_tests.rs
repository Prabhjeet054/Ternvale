use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::ConfigError;

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("ternvale-iso-{tag}-{}", std::process::id()));
        fs::create_dir_all(&dir).expect("temp dir");
        fs::write(dir.join("fw.fd"), b"x").expect("firmware");
        fs::write(dir.join("system.img"), vec![0u8; 4096]).expect("disk");
        disc(&dir.join("win.iso"), b"CD001", 17);
        disc(&dir.join("virtio-win.iso"), b"CD001", 18);
        disc(&dir.join("udf.iso"), b"BEA01", 17);
        disc(&dir.join("raw.img"), b"NOTCD", 17);
        Self(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).display().to_string()
    }

    /// A firmware config with one writable disk and `iso` as the `[iso]` body.
    fn config(&self, profile: &str, iso: &str) -> String {
        format!(
            "name = \"media\"\nprofile = \"{profile}\"\ncpus = 2\nram_mib = 4096\nfirmware = \"{fw}\"\n\
             serial_log = \"{serial}\"\n\n[[disks]]\npath = \"{disk}\"\nread_only = false\n\n[iso]\n{iso}",
            fw = self.path("fw.fd"),
            serial = self.path("serial.log"),
            disk = self.path("system.img"),
        )
    }

    fn slots(&self, installer: &str, drivers: &str) -> String {
        format!(
            "installer = \"{}\"\ndrivers = \"{}\"\n",
            self.path(installer),
            self.path(drivers)
        )
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            panic!("remove {}: {error}", self.0.display());
        }
    }
}

/// `sectors` 2048-byte sectors with `id` as sector 16's standard identifier.
fn disc(path: &Path, id: &[u8; 5], sectors: usize) {
    let mut bytes = vec![0u8; sectors * 2048];
    bytes[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(id);
    fs::write(path, bytes).expect("disc image");
}

#[test]
fn disc_format_reads_the_identifier_at_sector_16() {
    let dir = Dir::new("probe");
    let probe = |name: &str| disc_format(Path::new(&dir.path(name))).expect("probe");
    assert_eq!(probe("win.iso"), Some("iso9660"));
    assert_eq!(probe("udf.iso"), Some("udf"));
    assert_eq!(probe("raw.img"), None);
    assert_eq!(probe("system.img"), None, "shorter than 16 sectors");
}

#[test]
fn slots_attach_after_the_disks_read_only_installer_first() {
    let dir = Dir::new("order");
    let text = dir.config("windows", &dir.slots("win.iso", "virtio-win.iso"));
    let config = VmConfig::from_toml(&text).expect("windows with both slots");
    let devices = config.block_devices();
    let roles: Vec<_> = devices.iter().map(|d| d.role).collect();
    assert_eq!(
        roles,
        [BlockRole::Disk, BlockRole::Installer, BlockRole::Drivers]
    );
    assert_eq!(devices[1].path, PathBuf::from(dir.path("win.iso")));
    assert_eq!(devices[2].path, PathBuf::from(dir.path("virtio-win.iso")));
    assert!(!devices[0].read_only && !devices[0].is_optical());
    assert!(devices[1..].iter().all(|d| d.read_only && d.is_optical()));
    let again = VmConfig::from_toml(&config.to_toml().expect("serialize")).expect("reparse");
    assert_eq!(again, config);
}

#[test]
fn either_slot_is_optional_and_udf_only_images_are_accepted() {
    let dir = Dir::new("optional");
    let only_drivers = format!("drivers = \"{}\"\n", dir.path("virtio-win.iso"));
    let config = VmConfig::from_toml(&dir.config("windows", &only_drivers)).expect("drivers");
    assert_eq!(config.block_devices().len(), 2);
    let udf = format!("installer = \"{}\"\n", dir.path("udf.iso"));
    let config = VmConfig::from_toml(&dir.config("linux", &udf)).expect("udf installer");
    assert_eq!(config.block_devices()[1].role, BlockRole::Installer);
    let none = VmConfig::from_toml(&dir.config("linux", "")).expect("empty section");
    assert_eq!(none.block_devices().len(), 1);
}

#[test]
fn rejects_images_that_are_missing_not_discs_or_attached_twice() {
    let dir = Dir::new("reject");
    let error =
        VmConfig::from_toml(&dir.config("windows", &dir.slots("gone.iso", "virtio-win.iso")))
            .expect_err("missing installer");
    assert!(
        matches!(&error, ConfigError::MissingFile { field, .. } if field == "iso.installer"),
        "{error}"
    );
    let error = VmConfig::from_toml(&dir.config("windows", &dir.slots("win.iso", "raw.img")))
        .expect_err("not a disc");
    assert!(
        matches!(
            error,
            ConfigError::InvalidIso {
                field: "iso.drivers",
                ..
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("CD001"), "{error}");
    let error = VmConfig::from_toml(&dir.config("windows", &dir.slots("win.iso", "win.iso")))
        .expect_err("same image twice");
    assert!(
        error
            .to_string()
            .contains("already attached as iso.installer"),
        "{error}"
    );
    let twice = dir.config(
        "windows",
        &format!("installer = \"{}\"\n", dir.path("win.iso")),
    );
    let twice = twice.replace(&dir.path("system.img"), &dir.path("win.iso"));
    let error = VmConfig::from_toml(&twice).expect_err("disk and installer");
    assert!(
        error.to_string().contains("already attached as disks[0]"),
        "{error}"
    );
    let error =
        VmConfig::from_toml(&dir.config("windows", "cdrom = \"x\"\n")).expect_err("unknown key");
    assert!(matches!(error, ConfigError::Parse { .. }), "{error}");
}
