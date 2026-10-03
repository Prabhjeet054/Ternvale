//! What CPU 0 loads before it starts: a Linux Image or the UEFI firmware.
//!
//! A direct boot builds the DTB and the Linux placement until they agree.
//! A firmware boot maps the code bank and opens the variable flash; its DTB
//! goes at the first byte of RAM (see [`crate::firmware`]).

use std::path::Path;

use super::MachineError;
use crate::fdt::{build_fdt, GuestFdt};
use crate::firmware::{HvWindow, VarsFlash};
use crate::linux::{place, LinuxLayout};
use crate::memory::GuestMemory;
use crate::platform::{FLASH_VARS_BASE, RAM_BASE};

/// How CPU 0 starts.
pub(super) enum Boot<'a> {
    /// Linux arm64 boot protocol.
    Linux { kernel: &'a [u8], initrd: &'a [u8] },
    /// EDK2 reset vector at GPA 0.
    Firmware,
}

pub(super) struct Images<'a> {
    pub boot: Boot<'a>,
    pub dtb: &'a [u8],
    pub ram_size: u64,
}

/// Host files for one boot.
pub(super) enum Inputs {
    Linux {
        kernel: Vec<u8>,
        initrd: Vec<u8>,
    },
    /// `fw_cfg`: the DTB gets the fw_cfg node (`firmware_tables = "acpi"`).
    Firmware {
        code: Vec<u8>,
        fw_cfg: bool,
    },
}

impl Inputs {
    #[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(name = %config.name))]
    pub(super) fn read(config: &ternvale_config::VmConfig) -> Result<Self, MachineError> {
        if let Some(path) = &config.firmware {
            let tables = config.effective_firmware_tables();
            tracing::info!(
                target: "ternvale::boot",
                firmware = %path.display(),
                firmware_tables = %tables,
                explicit = config.firmware_tables.is_some(),
                "firmware boot: UEFI instead of a direct kernel boot"
            );
            return Ok(Self::Firmware {
                code: super::read_file("firmware", path)?,
                fw_cfg: tables == ternvale_config::FirmwareTables::Acpi,
            });
        }
        let kernel = super::read_file("kernel", &config.kernel)?;
        let initrd = match &config.initrd {
            Some(path) => super::read_file("initrd", path)?,
            None => Vec::new(),
        };
        Ok(Self::Linux { kernel, initrd })
    }

    /// Map the firmware code bank (no-op for a direct boot).
    pub(super) fn map(
        &self,
        memory: &mut GuestMemory,
        vm: &ternvale_hv::Vm,
    ) -> Result<(), MachineError> {
        if let Self::Firmware { code, .. } = self {
            crate::firmware::map_code(memory, vm, code)?;
        }
        Ok(())
    }

    /// The DTB for this boot.
    pub(super) fn dtb(
        &self,
        cmdline: &str,
        cpus: u32,
        ram_size: u64,
    ) -> Result<Vec<u8>, MachineError> {
        match self {
            Self::Linux { kernel, initrd } => {
                let (dtb, layout) = boot_images(cmdline, cpus, ram_size, kernel, initrd)?;
                tracing::debug!(
                    target: "ternvale::boot",
                    kernel = format!("{:#x}", layout.kernel),
                    dtb = format!("{:#x}", layout.dtb),
                    "boot images placed"
                );
                Ok(dtb)
            }
            Self::Firmware { fw_cfg, .. } => {
                let dtb = build_fdt(&GuestFdt {
                    bootargs: String::new(),
                    ram_base: RAM_BASE,
                    ram_size,
                    initrd_start: 0,
                    initrd_end: 0,
                    cpu_count: cpus,
                    firmware: true,
                    fw_cfg: *fw_cfg,
                })?;
                crate::firmware::check_layout(ram_size, dtb.len() as u64)?;
                Ok(dtb)
            }
        }
    }

    pub(super) fn images<'a>(&'a self, dtb: &'a [u8], ram_size: u64) -> Images<'a> {
        let boot = match self {
            Self::Linux { kernel, initrd } => Boot::Linux { kernel, initrd },
            Self::Firmware { .. } => Boot::Firmware,
        };
        Images {
            boot,
            dtb,
            ram_size,
        }
    }
}

/// The variable flash for a firmware boot, backed by `nvram`.
#[tracing::instrument(level = "debug", target = "ternvale::boot", skip_all, fields(nvram = %nvram.display()))]
pub(super) fn vars_flash(nvram: &Path) -> Result<VarsFlash, MachineError> {
    Ok(VarsFlash::open(
        nvram,
        Box::new(HvWindow::new(FLASH_VARS_BASE)),
    )?)
}

fn boot_images(
    cmdline: &str,
    cpus: u32,
    ram_size: u64,
    kernel: &[u8],
    initrd: &[u8],
) -> Result<(Vec<u8>, LinuxLayout), MachineError> {
    let header = crate::linux::parse_header(kernel)?;
    let mut dtb;
    let mut layout = None;
    for _ in 0..3 {
        let guess = layout.unwrap_or(LinuxLayout {
            kernel: 0,
            kernel_bytes: 0,
            initrd: 0,
            initrd_bytes: initrd.len() as u64,
            dtb: 0,
            dtb_bytes: 0,
        });
        dtb = build_fdt(&GuestFdt {
            bootargs: cmdline.to_string(),
            ram_base: RAM_BASE,
            ram_size,
            initrd_start: guess.initrd,
            initrd_end: guess.initrd.saturating_add(initrd.len() as u64),
            cpu_count: cpus,
            firmware: false,
            fw_cfg: false,
        })?;
        let next = place(
            RAM_BASE,
            ram_size,
            &header,
            kernel.len() as u64,
            initrd.len() as u64,
            dtb.len() as u64,
        )?;
        if layout == Some(next) {
            return Ok((dtb, next));
        }
        layout = Some(next);
    }
    tracing::error!(target: "ternvale::boot", "dtb placement did not settle");
    Err(MachineError::Placement)
}
