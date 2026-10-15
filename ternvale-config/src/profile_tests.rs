use std::fs;
use std::path::PathBuf;

use super::*;
use crate::TpmBackendKind;

struct Dir(PathBuf);

impl Dir {
    fn new(tag: &str) -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!(
            "ternvale-profile-{tag}-{}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).expect("temp dir");
        fs::write(dir.join("fw.fd"), b"x").expect("firmware");
        Self(dir)
    }

    fn path(&self, name: &str) -> String {
        self.0.join(name).display().to_string()
    }

    /// A windows-profile document with `extra` lines appended.
    fn windows(&self, extra: &str) -> String {
        format!(
            "name = \"win\"\nprofile = \"windows\"\nfirmware = \"{}\"\nnvram = \"{}\"\nserial_log = \"{}\"\n{extra}",
            self.path("fw.fd"),
            self.path("vars.fd"),
            self.path("serial.log"),
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

/// Every value `profile = "windows"` decides, read through the accessors the
/// VMM and CLI use.
fn assert_windows_flags(config: &VmConfig, dir: &Dir) {
    assert_eq!(config.profile, Profile::Windows);
    assert!(config.cpus >= WINDOWS_MIN_CPUS, "cpus {}", config.cpus);
    assert!(
        config.ram_mib >= WINDOWS_MIN_RAM_MIB,
        "ram {}",
        config.ram_mib
    );
    assert_eq!(config.firmware_tables, None, "acpi is derived, not written");
    assert_eq!(config.effective_firmware_tables(), FirmwareTables::Acpi);
    assert!(config.tpm_enabled());
    assert_eq!(config.tpm_section().backend, TpmBackendKind::Auto);
    let nvram = PathBuf::from(dir.path("vars.fd"));
    assert_eq!(config.nvram_path().expect("nvram"), Some(nvram.clone()));
    let mut state = nvram.into_os_string();
    state.push(crate::TPM_STATE_SUFFIX);
    assert_eq!(
        config.tpm_state_path().expect("tpm state"),
        Some(PathBuf::from(state))
    );
    assert_eq!(config.virtio_transport(), VirtioTransport::Pci);
    assert!(config.iso_boots_first());
}

#[test]
fn windows_sets_every_derived_flag_and_defaults_to_the_windows_11_minimums() {
    let dir = Dir::new("defaults");
    let config = VmConfig::from_toml(&dir.windows("")).expect("windows defaults");
    assert_eq!(
        (config.cpus, config.ram_mib),
        (WINDOWS_MIN_CPUS, WINDOWS_MIN_RAM_MIB)
    );
    assert_eq!((config.cpus, config.ram_mib), (2, 4096));
    assert_windows_flags(&config, &dir);
    let bigger = VmConfig::from_toml(&dir.windows("cpus = 8\nram_mib = 16384\n")).expect("bigger");
    assert_windows_flags(&bigger, &dir);
    let reparsed = VmConfig::from_toml(&config.to_toml().expect("serialize")).expect("reparse");
    assert_windows_flags(&reparsed, &dir);
}

/// The windows config as a program (the boot harness, a future app) builds
/// it: a struct literal checked with `validate`, not parsed TOML.
fn windows_in_code(dir: &Dir, cpus: u32, ram_mib: u64) -> VmConfig {
    VmConfig {
        name: "win".to_string(),
        profile: Profile::Windows,
        iso: None,
        cpus,
        ram_mib,
        kernel: PathBuf::new(),
        initrd: None,
        cmdline: String::new(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: PathBuf::from(dir.path("serial.log")),
        firmware: Some(PathBuf::from(dir.path("fw.fd"))),
        nvram: Some(PathBuf::from(dir.path("vars.fd"))),
        firmware_tables: None,
        vsock: None,
        tpm: None,
    }
}

#[test]
fn two_gib_of_ram_is_rejected_with_a_message_naming_the_ram_requirement() {
    let dir = Dir::new("ram");
    let parsed = VmConfig::from_toml(&dir.windows("ram_mib = 2048\n")).expect_err("toml 2 GiB");
    let built = windows_in_code(&dir, 2, 2048)
        .validate()
        .expect_err("code 2 GiB");
    for error in [parsed, built] {
        assert!(
            matches!(
                error,
                ConfigError::BelowProfileMinimum {
                    profile: Profile::Windows,
                    field: "ram_mib",
                    minimum: 4096,
                    got: 2048,
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(
            error.to_string(),
            "profile = \"windows\" needs ram_mib of at least 4096, got 2048 \
             (Windows 11 requires at least 4 GB of memory)"
        );
    }
    let just_under = windows_in_code(&dir, 2, 4080)
        .validate()
        .expect_err("4080 MiB");
    assert!(
        error_names_ram(&just_under),
        "4080 MiB is under 4 GiB: {just_under}"
    );
    windows_in_code(&dir, 2, 4096)
        .validate()
        .expect("exactly 4 GiB");
    assert_windows_flags(&windows_in_code(&dir, 2, 4096), &dir);
}

fn error_names_ram(error: &ConfigError) -> bool {
    error.to_string().contains("ram_mib of at least 4096")
        && error.to_string().contains("4 GB of memory")
}

#[test]
fn explicit_sizes_win_over_the_profile_defaults_and_round_trip() {
    let dir = Dir::new("explicit");
    let config = VmConfig::from_toml(&dir.windows("cpus = 4\nram_mib = 8192\n")).expect("explicit");
    assert_eq!((config.cpus, config.ram_mib), (4, 8192));
    let text = config.to_toml().expect("serialize");
    assert!(text.contains("profile = \"windows\""), "{text}");
    assert_eq!(VmConfig::from_toml(&text).expect("reparse"), config);
    let partial = VmConfig::from_toml(&dir.windows("cpus = 3\n")).expect("cpus only");
    assert_eq!((partial.cpus, partial.ram_mib), (3, 4096));
}

#[test]
fn one_vcpu_is_rejected_with_a_message_naming_the_cpu_requirement() {
    let dir = Dir::new("minimum");
    let error = VmConfig::from_toml(&dir.windows("cpus = 1\n")).expect_err("1 cpu");
    assert!(!error_names_ram(&error), "{error}");
    let text = error.to_string();
    assert!(text.contains("needs cpus of at least 2, got 1"), "{text}");
    assert!(
        text.contains("Windows 11 requires at least 2 virtual processors"),
        "{text}"
    );
    VmConfig::from_toml(&dir.windows("cpus = 2\nram_mib = 4096\n")).expect("exactly the minimums");
}

#[test]
fn windows_rejects_settings_that_contradict_the_preset() {
    let dir = Dir::new("conflict");
    let error = VmConfig::from_toml(&dir.windows("firmware_tables = \"fdt\"\n")).expect_err("fdt");
    assert!(
        matches!(
            error,
            ConfigError::ProfileConflict {
                field: "firmware_tables = \"fdt\"",
                ..
            }
        ),
        "{error}"
    );
    assert!(error.to_string().contains("not a device tree"), "{error}");
    let error = VmConfig::from_toml(&dir.windows("[tpm]\nenabled = false\n")).expect_err("no tpm");
    assert!(
        error.to_string().contains("Windows 11 requires a TPM 2.0"),
        "{error}"
    );
    let direct = format!(
        "name = \"win\"\nprofile = \"windows\"\nkernel = \"{}\"\nserial_log = \"{}\"\n",
        dir.path("fw.fd"),
        dir.path("serial.log")
    );
    let error = VmConfig::from_toml(&direct).expect_err("no firmware");
    assert!(
        matches!(error, ConfigError::ProfileNeedsFirmware { .. }),
        "{error}"
    );
    VmConfig::from_toml(&dir.windows("firmware_tables = \"acpi\"\n[tpm]\nbackend = \"stub\"\n"))
        .expect("acpi and a stub TPM are allowed (the stub is only warned about)");
}

#[test]
fn linux_is_the_default_and_keeps_cpus_and_ram_required() {
    let dir = Dir::new("linux");
    let text = dir.windows("").replace("profile = \"windows\"\n", "");
    let error = VmConfig::from_toml(&text).expect_err("no cpus");
    assert!(
        error.to_string().contains("missing field `cpus`"),
        "{error}"
    );
    let config = VmConfig::from_toml(&format!("{text}cpus = 1\nram_mib = 256\n")).expect("linux");
    assert_eq!(config.profile, Profile::Linux);
    assert_eq!(config.virtio_transport(), VirtioTransport::Mmio);
    assert!(!config.iso_boots_first());
    assert!(!config.to_toml().expect("serialize").contains("profile ="));
    let explicit = text.clone() + "profile = \"linux\"\ncpus = 1\nram_mib = 256\n";
    assert_eq!(
        VmConfig::from_toml(&explicit).expect("explicit linux"),
        config
    );
    let error =
        VmConfig::parse(&text.replace("name", "profile = \"macos\"\nname")).expect_err("bad");
    assert!(error.to_string().contains("macos"), "{error}");
}

/// The Step 18 boot harness config (`tests/boot/initrd.rs`): a direct kernel
/// boot of `Image` + `initramfs.cpio`, 1 vCPU, 256 MiB, no firmware.
fn step18_harness(dir: &Dir, profile: Profile) -> VmConfig {
    VmConfig {
        name: "boot".to_string(),
        profile,
        iso: None,
        cpus: 1,
        ram_mib: 256,
        kernel: PathBuf::from(dir.path("Image")),
        initrd: Some(PathBuf::from(dir.path("initramfs.cpio"))),
        cmdline: String::new(),
        boot_disk: false,
        disks: Vec::new(),
        nics: Vec::new(),
        serial_log: PathBuf::from(dir.path("guest-serial.log")),
        firmware: None,
        nvram: None,
        firmware_tables: None,
        vsock: None,
        tpm: None,
    }
}

#[test]
fn linux_leaves_the_step18_harness_config_unchanged() {
    let dir = Dir::new("step18");
    for name in ["Image", "initramfs.cpio"] {
        fs::write(dir.0.join(name), b"x").expect("boot file");
    }
    let config = step18_harness(&dir, Profile::Linux);
    assert_eq!(config.profile, Profile::default());
    config
        .validate()
        .expect("the harness config is still valid");
    assert_eq!((config.cpus, config.ram_mib), (1, 256), "no profile sizes");
    assert_eq!(config.effective_firmware_tables(), FirmwareTables::Fdt);
    assert_eq!(config.virtio_transport(), VirtioTransport::Mmio);
    assert!(!config.iso_boots_first());
    assert_eq!(config.nvram_path().expect("nvram"), None);
    assert_eq!(config.tpm_state_path().expect("tpm state"), None);

    let text = config.to_toml().expect("serialize");
    let golden = format!(
        "name = \"boot\"\ncpus = 1\nram_mib = 256\nkernel = \"{}\"\ninitrd = \"{}\"\ncmdline = \"\"\nserial_log = \"{}\"\n",
        dir.path("Image"),
        dir.path("initramfs.cpio"),
        dir.path("guest-serial.log"),
    );
    assert_eq!(text, golden, "the document has no profile key");
    assert_eq!(VmConfig::from_toml(&text).expect("reparse"), config);
    let explicit = text.replacen('\n', "\nprofile = \"linux\"\n", 1);
    assert_eq!(VmConfig::from_toml(&explicit).expect("explicit"), config);

    let error = step18_harness(&dir, Profile::Windows)
        .validate()
        .expect_err("windows needs firmware");
    assert!(
        matches!(error, ConfigError::ProfileNeedsFirmware { .. }),
        "{error}"
    );
}

#[test]
fn iso9660_is_detected_by_the_cd001_identifier_in_sector_16() {
    let dir = Dir::new("iso");
    let mut image = vec![0u8; 18 * 2048];
    image[16 * 2048] = 1;
    image[16 * 2048 + 1..16 * 2048 + 6].copy_from_slice(b"CD001");
    fs::write(dir.0.join("a.iso"), &image).expect("iso");
    assert!(is_iso9660(&dir.0.join("a.iso")).expect("probe"));
    image[16 * 2048 + 1] = b'X';
    fs::write(dir.0.join("b.img"), &image).expect("img");
    assert!(!is_iso9660(&dir.0.join("b.img")).expect("probe"));
    fs::write(dir.0.join("tiny.img"), b"CD001").expect("tiny");
    assert!(!is_iso9660(&dir.0.join("tiny.img")).expect("probe"));
    assert!(is_iso9660(&dir.0.join("missing")).is_err());
}

#[test]
fn names_are_lowercase() {
    assert_eq!(Profile::Windows.to_string(), "windows");
    assert_eq!(Profile::Linux.to_string(), "linux");
    assert_eq!(VirtioTransport::Pci.to_string(), "virtio-pci");
    assert_eq!(VirtioTransport::Mmio.to_string(), "virtio-mmio");
}
