//! QEMU fw_cfg over MMIO at [`crate::platform::FW_CFG_BASE`] (QEMU `VIRT_FW_CFG`).
//!
//! Present only on firmware boots with `firmware_tables = "acpi"`, to hand
//! EDK2 the ACPI linker/loader files (see `ternvale_acpi::LoaderBlobs`).
//! Register layout (QEMU `docs/specs/fw_cfg.rst`, as EDK2
//! `OvmfPkg/Library/QemuFwCfgLib/QemuFwCfgLibMmio.c` drives it):
//! - **Data** at `+0x0`, read with 1, 2, 4, or 8 byte loads. Each load
//!   returns the next bytes of the selected item in stream order, so a
//!   little-endian guest stores them in item order. Past the end, reads
//!   return 0.
//! - **Selector** at `+0x8`, a 16-bit big-endian write. It picks an item and
//!   rewinds it.
//! - **No DMA register.** QEMU's `reg` is `0x18` bytes, with the DMA address
//!   at `+0x10`. Ternvale's is `0x10`, and the feature word (item 1) leaves
//!   the DMA bit clear. EDK2 edk2-stable202308 checked the feature bit. Newer
//!   EDK2 (`QemuFwCfgMmioPei.c`, master 2026-10) enables DMA whenever `reg` is
//!   at least `0x18`, without reading the feature word. So only the shorter
//!   `reg` keeps both on the data register. An access at `+0x10` falls outside
//!   the device and the MMIO bus logs it as unmapped.
//!
//! Items: `0x0000` signature `"QEMU"`, `0x0001` features (`1`, traditional
//! interface only), `0x0019` file directory, and files from `0x0020` in the
//! order given. The directory is a big-endian count, then
//! `{size: be32, select: be16, reserved: u16, name: [u8; 56]}` per file.
//! Unknown items read as zeros, as in QEMU. EDK2 probes some of them
//! (kernel size, boot menu), and zero means "not provided".

use crate::firmware::FirmwareError;
use crate::mmio::MmioDevice;

/// fw_cfg register window: data and selector, no DMA address register.
pub const FW_CFG_REG_SIZE: u64 = 0x10;

const DATA: u64 = 0x0;
const SELECTOR: u64 = 0x8;
const ITEM_SIGNATURE: u16 = 0x0000;
const ITEM_FEATURES: u16 = 0x0001;
const ITEM_FILE_DIR: u16 = 0x0019;
const FIRST_FILE: u16 = 0x0020;
/// `FW_CFG_VERSION`: the traditional (non-DMA) interface.
const FEATURE_TRADITIONAL: u32 = 1;
/// Name field, NUL included (`FW_CFG_MAX_FILE_PATH`).
const NAME_LEN: usize = 56;
/// Files Ternvale may offer before selectors reach QEMU's reserved ranges.
const MAX_FILES: usize = 0x1000;

/// One fw_cfg device holding a fixed set of read-only files.
#[derive(Debug)]
pub struct FwCfg {
    items: Vec<(u16, String, Vec<u8>)>,
    selected: Option<usize>,
    key: u16,
    offset: usize,
}

impl FwCfg {
    /// A device offering `files` (`name`, contents) from selector `0x20` on.
    /// Names must be 1 to 55 bytes without NUL and unique.
    #[tracing::instrument(level = "debug", target = "ternvale::fw_cfg", skip_all, fields(files = files.len()))]
    pub fn new(files: Vec<(String, Vec<u8>)>) -> Result<Self, FirmwareError> {
        if files.len() > MAX_FILES {
            return Err(bad_file(
                "*",
                format!("{} files, at most {MAX_FILES}", files.len()),
            ));
        }
        let mut dir = (files.len() as u32).to_be_bytes().to_vec();
        let mut items = vec![
            (ITEM_SIGNATURE, "signature".to_string(), b"QEMU".to_vec()),
            (
                ITEM_FEATURES,
                "features".to_string(),
                FEATURE_TRADITIONAL.to_le_bytes().to_vec(),
            ),
        ];
        for (index, (name, bytes)) in files.into_iter().enumerate() {
            if name.is_empty() || name.len() >= NAME_LEN || name.contains('\0') {
                return Err(bad_file(
                    &name,
                    format!("name must be 1..{NAME_LEN} bytes without NUL"),
                ));
            }
            if items.iter().any(|(_, existing, _)| *existing == name) {
                return Err(bad_file(&name, "duplicate name".to_string()));
            }
            let Ok(size) = u32::try_from(bytes.len()) else {
                return Err(bad_file(
                    &name,
                    format!("{} bytes do not fit a u32", bytes.len()),
                ));
            };
            let select = FIRST_FILE + index as u16;
            dir.extend_from_slice(&size.to_be_bytes());
            dir.extend_from_slice(&select.to_be_bytes());
            dir.extend_from_slice(&[0, 0]);
            let mut field = [0u8; NAME_LEN];
            field[..name.len()].copy_from_slice(name.as_bytes());
            dir.extend_from_slice(&field);
            tracing::debug!(target: "ternvale::fw_cfg", name = %name, select = %format!("{select:#06x}"), size, "fw_cfg file");
            items.push((select, name, bytes));
        }
        items.push((ITEM_FILE_DIR, "file-dir".to_string(), dir));
        let device = Self {
            items,
            selected: None,
            key: 0,
            offset: 0,
        };
        tracing::info!(
            target: "ternvale::fw_cfg",
            files = device.items.len() - 3,
            names = %device.file_names().collect::<Vec<_>>().join(","),
            "fw_cfg ready"
        );
        Ok(device)
    }

    /// The offered file names, in selector order.
    pub(crate) fn file_names(&self) -> impl Iterator<Item = &str> {
        self.items
            .iter()
            .filter(|(key, ..)| *key >= FIRST_FILE)
            .map(|(_, name, _)| name.as_str())
    }

    fn select(&mut self, key: u16) {
        self.key = key;
        self.offset = 0;
        self.selected = self.items.iter().position(|(item, ..)| *item == key);
        match self.selected {
            Some(index) => tracing::debug!(
                target: "ternvale::fw_cfg",
                key = %format!("{key:#06x}"),
                item = %self.items[index].1,
                size = self.items[index].2.len(),
                "fw_cfg item selected"
            ),
            None => {
                tracing::debug!(target: "ternvale::fw_cfg", key = %format!("{key:#06x}"), "fw_cfg item not provided; reads return 0")
            }
        }
    }

    fn read_data(&mut self, size: u8) -> u64 {
        let mut bytes = [0u8; 8];
        let want = usize::from(size).min(8);
        if let Some(index) = self.selected {
            let item = &self.items[index].2;
            let start = self.offset.min(item.len());
            let end = (start + want).min(item.len());
            bytes[..end - start].copy_from_slice(&item[start..end]);
        }
        self.offset = self.offset.saturating_add(want);
        u64::from_le_bytes(bytes)
    }
}

impl MmioDevice for FwCfg {
    fn name(&self) -> &str {
        "fw-cfg"
    }

    fn read(&mut self, offset: u64, size: u8) -> u64 {
        let value = if offset == DATA && matches!(size, 1 | 2 | 4 | 8) {
            self.read_data(size)
        } else {
            tracing::warn!(target: "ternvale::fw_cfg", offset = %format!("{offset:#x}"), size, "fw_cfg read outside the data register; returning 0");
            0
        };
        tracing::trace!(
            target: "ternvale::fw_cfg",
            key = %format!("{:#06x}", self.key),
            offset = %format!("{offset:#x}"),
            size,
            value = %format!("{value:#x}"),
            item_offset = self.offset,
            "fw_cfg read"
        );
        value
    }

    fn write(&mut self, offset: u64, size: u8, val: u64) {
        tracing::trace!(target: "ternvale::fw_cfg", offset = %format!("{offset:#x}"), size, value = %format!("{val:#x}"), "fw_cfg write");
        match (offset, size) {
            (SELECTOR, 2) => self.select((val as u16).swap_bytes()),
            _ => {
                tracing::warn!(target: "ternvale::fw_cfg", offset = %format!("{offset:#x}"), size, "fw_cfg write ignored (data writes and odd-sized selects are not supported)")
            }
        }
    }
}

fn bad_file(name: &str, reason: String) -> FirmwareError {
    tracing::error!(target: "ternvale::fw_cfg", name, %reason, "fw_cfg file rejected");
    FirmwareError::FwCfgFile {
        name: name.to_string(),
        reason,
    }
}

#[cfg(test)]
#[path = "fw_cfg_tests.rs"]
mod tests;
