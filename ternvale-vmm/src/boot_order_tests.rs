use std::path::PathBuf;

use super::*;

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        let dir =
            std::env::temp_dir().join(format!("ternvale-bootorder-{tag}-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let mut iso = vec![0u8; 17 * 2048];
        iso[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(b"CD001");
        std::fs::write(dir.join("install.iso"), &iso).expect("iso");
        std::fs::write(dir.join("drivers.iso"), &iso).expect("drivers iso");
        std::fs::write(dir.join("system.img"), vec![0u8; 64 * 1024]).expect("disk");
        Self(dir)
    }

    /// `config` plus an `[iso]` section with the given slots.
    fn with_iso(&self, profile: &str, disks: &[&str], installer: bool, drivers: bool) -> VmConfig {
        let mut config = self.config(profile, disks);
        config.iso = Some(ternvale_config::IsoSection {
            installer: installer.then(|| self.0.join("install.iso")),
            drivers: drivers.then(|| self.0.join("drivers.iso")),
        });
        config
    }

    fn config(&self, profile: &str, disks: &[&str]) -> VmConfig {
        let mut text = format!(
            "name = \"b\"\nprofile = \"{profile}\"\ncpus = 2\nram_mib = 4096\nfirmware = \"/fw.fd\"\nserial_log = \"/tmp/s.log\"\n"
        );
        for disk in disks {
            text.push_str(&format!(
                "[[disks]]\npath = \"{}\"\nread_only = false\n",
                self.0.join(disk).display()
            ));
        }
        VmConfig::parse(&text).expect("parse")
    }
}

impl Drop for Dir {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_dir_all(&self.0) {
            panic!("remove {}: {error}", self.0.display());
        }
    }
}

#[test]
fn ofw_paths_follow_qemu_boot_order_lib() {
    assert_eq!(
        disk_ofw_path(VirtioTransport::Pci, 0).expect("pci 0"),
        "/pci@i0cf8/scsi@1/disk@0,0"
    );
    assert_eq!(
        disk_ofw_path(VirtioTransport::Pci, 14).expect("pci 14"),
        "/pci@i0cf8/scsi@f/disk@0,0"
    );
    assert!(disk_ofw_path(VirtioTransport::Pci, 31).is_err());
    assert_eq!(
        disk_ofw_path(VirtioTransport::Mmio, 2).expect("mmio 2"),
        "/virtio-mmio@000000000a000400/disk@0,0"
    );
    assert!(disk_ofw_path(VirtioTransport::Mmio, 32).is_err());
    assert_eq!(pci_disk_bdf(0), Some(Bdf::new(1)));
    assert_eq!(pci_disk_bdf(30), Some(Bdf::new(31)));
    assert_eq!(pci_disk_bdf(31), None);
}

#[test]
fn windows_lists_the_iso_first_then_every_other_disk_nul_terminated() {
    let dir = Dir::new("windows");
    let config = dir.config("windows", &["system.img", "install.iso"]);
    let (name, bytes) = file(&config).expect("file").expect("iso attached");
    assert_eq!(name, BOOTORDER_FILE);
    assert_eq!(
        bytes,
        b"/pci@i0cf8/scsi@2/disk@0,0\n/pci@i0cf8/scsi@1/disk@0,0\0".to_vec()
    );
}

#[test]
fn windows_flags_reach_the_vmm_as_acpi_with_tpm2_pci_disks_and_a_bootorder() {
    let dir = Dir::new("derived");
    let config = dir.config("windows", &["system.img", "install.iso"]);
    assert_eq!(
        config.effective_firmware_tables(),
        ternvale_config::FirmwareTables::Acpi
    );
    assert!(
        crate::acpi_check::vm_acpi_config(&config).tpm.is_some(),
        "TPM2 table and \\_SB.TPM2"
    );
    assert_eq!(config.virtio_transport(), VirtioTransport::Pci);
    for index in 0..config.disks.len() {
        assert!(pci_disk_bdf(index).is_some(), "disk {index} has a PCI slot");
    }
    assert!(file(&config).expect("file").is_some(), "bootorder");
}

#[test]
fn installer_slot_boots_first_and_the_driver_disc_is_left_out() {
    let dir = Dir::new("slots");
    let config = dir.with_iso("windows", &["system.img"], true, true);
    let (_, bytes) = file(&config).expect("file").expect("installer attached");
    // block_devices: system.img 00:01.0, installer 00:02.0, drivers 00:03.0.
    assert_eq!(
        bytes,
        b"/pci@i0cf8/scsi@2/disk@0,0\n/pci@i0cf8/scsi@1/disk@0,0\0".to_vec()
    );
    let config = dir.with_iso("windows", &["install.iso", "system.img"], true, false);
    let (_, bytes) = file(&config).expect("file").expect("two isos");
    assert_eq!(
        bytes,
        b"/pci@i0cf8/scsi@3/disk@0,0\n/pci@i0cf8/scsi@1/disk@0,0\n/pci@i0cf8/scsi@2/disk@0,0\0"
            .to_vec(),
        "installer slot, then the ISO disk, then the other disk"
    );
    let drivers_only = dir.with_iso("windows", &["system.img"], false, true);
    assert_eq!(
        file(&drivers_only).expect("file"),
        None,
        "nothing to boot first"
    );
}

#[test]
fn no_file_without_an_iso_or_for_linux() {
    let dir = Dir::new("none");
    assert_eq!(
        file(&dir.config("windows", &["system.img"])).expect("file"),
        None
    );
    assert_eq!(
        file(&dir.config("linux", &["system.img", "install.iso"])).expect("file"),
        None
    );
}

#[test]
fn an_unreadable_disk_is_a_named_read_error() {
    let dir = Dir::new("missing");
    let error = file(&dir.config("windows", &["gone.iso"])).expect_err("missing disk");
    assert!(
        matches!(error, MachineError::Read { what: "disk", .. }),
        "{error}"
    );
}
