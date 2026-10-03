//! Driving the UEFI shell's `dmem` over the serial console and reading its
//! output.
//!
//! `dmem <addr> <len>` (both hex) prints a header and
//! `  ADDR: xx xx xx xx xx xx xx xx-xx ... xx  *ascii*` lines. `dmem` with no
//! arguments dumps the EFI system table the same way, then prints
//! `Valid EFI Header at Address <16 hex digits>` and one labelled line per
//! well-known configuration table. Those labels are not trusted: the
//! ArmVirtQemu shell prints `DTB Table 0` even when EDK2 has installed
//! `gFdtTableGuid` (seen with QEMU `virt,acpi=off` too). So
//! [`Shell::config_tables`] reads the raw `EFI_CONFIGURATION_TABLE` array
//! instead (UEFI 2.10 §4.3: `NumberOfTableEntries` at system table +0x68,
//! `ConfigurationTable` at +0x70; 24-byte entries of GUID, then pointer).

use std::path::Path;
use std::sync::atomic::AtomicBool;

use ternvale_devices::Step;

use crate::common::drive;
use crate::firmware::{expect, send};

/// `EFI_ACPI_20_TABLE_GUID` 8868e871-e4f1-11d3-bc22-0080c73c8881, as stored.
pub const ACPI_20_GUID: [u8; 16] = [
    0x71, 0xe8, 0x68, 0x88, 0xf1, 0xe4, 0xd3, 0x11, 0xbc, 0x22, 0x00, 0x80, 0xc7, 0x3c, 0x88, 0x81,
];
/// EDK2 `gFdtTableGuid` b1b621d5-f19c-41a5-830b-d9152c69aae0, as stored.
pub const FDT_GUID: [u8; 16] = [
    0xd5, 0x21, 0xb6, 0xb1, 0x9c, 0xf1, 0xa5, 0x41, 0x83, 0x0b, 0xd9, 0x15, 0x2c, 0x69, 0xaa, 0xe0,
];
const SYSTEM_TABLE_LEN: usize = 0x78;
const ENTRIES_OFFSET: usize = 0x68;
const TABLE_OFFSET: usize = 0x70;
const ENTRY_LEN: usize = 24;
const MAX_ENTRIES: u64 = 64;

/// The UEFI shell on the serial console.
pub struct Shell<'a> {
    pub serial_log: &'a Path,
    pub input: &'a mut std::fs::File,
    pub cancel: &'a AtomicBool,
    pub host: &'a Path,
}

impl Shell<'_> {
    pub fn run(&mut self, steps: Vec<Step>) -> Result<(), String> {
        drive(self.serial_log, self.input, self.cancel, self.host, steps)
    }

    /// Send `command` and wait for the next prompt; return the new output.
    /// One expect only: only the first expect after a send skips old text.
    fn command(&mut self, command: &str) -> Result<String, String> {
        let before = strip(&read(self.serial_log)?).len();
        let line = format!("{command}\r");
        let chunks: Vec<&[u8]> = line.as_bytes().chunks(16).collect();
        let mut steps = send(&chunks);
        steps.push(expect("Shell>", 30));
        self.run(steps)?;
        let text = strip(&read(self.serial_log)?);
        Ok(text.get(before..).unwrap_or("").to_string())
    }

    /// `dmem <gpa> <len>` and the bytes it printed.
    pub fn dmem(&mut self, gpa: u64, len: usize) -> Result<Vec<u8>, String> {
        let text = self.command(&format!("dmem {gpa:x} {len:x}"))?;
        let bytes = dump_bytes(&text);
        if bytes.len() < len {
            return Err(format!(
                "dmem {gpa:#x} {len:#x} returned {} bytes",
                bytes.len()
            ));
        }
        Ok(bytes[..len].to_vec())
    }

    /// Every `(VendorGuid, VendorTable)` in the system table's configuration
    /// table array.
    pub fn config_tables(&mut self) -> Result<Vec<([u8; 16], u64)>, String> {
        let text = self.command("dmem")?;
        let end = text.find("Valid EFI Header").unwrap_or(text.len());
        let system = dump_bytes(&text[..end]);
        if system.len() < SYSTEM_TABLE_LEN || &system[..8] != b"IBI SYST" {
            return Err(format!(
                "dmem printed no EFI system table ({} bytes)",
                system.len()
            ));
        }
        let count = u64_at(&system, ENTRIES_OFFSET);
        let table = u64_at(&system, TABLE_OFFSET);
        if count == 0 || count > MAX_ENTRIES {
            return Err(format!("system table claims {count} configuration tables"));
        }
        let raw = self.dmem(table, count as usize * ENTRY_LEN)?;
        let entries: Vec<([u8; 16], u64)> = raw
            .chunks_exact(ENTRY_LEN)
            .map(|entry| {
                let mut guid = [0u8; 16];
                guid.copy_from_slice(&entry[..16]);
                (guid, u64_at(entry, 16))
            })
            .collect();
        tracing::info!(
            target: "ternvale::boot",
            system_table_entries = count,
            table = %format!("{table:#x}"),
            acpi20 = ?find(&entries, &ACPI_20_GUID).map(|a| format!("{a:#x}")),
            fdt = ?find(&entries, &FDT_GUID).map(|a| format!("{a:#x}")),
            "uefi configuration tables"
        );
        Ok(entries)
    }
}

/// The table registered under `guid`, if any.
pub fn find(entries: &[([u8; 16], u64)], guid: &[u8; 16]) -> Option<u64> {
    entries
        .iter()
        .find(|(entry, _)| entry == guid)
        .map(|&(_, table)| table)
}

pub fn read(path: &Path) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|err| format!("read {}: {err}", path.display()))
}

pub fn u64_at(bytes: &[u8], at: usize) -> u64 {
    let mut value = [0u8; 8];
    value.copy_from_slice(&bytes[at..at + 8]);
    u64::from_le_bytes(value)
}

/// `text` without CSI escape sequences or carriage returns.
pub fn strip(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\u{1b}' => {
                if chars.peek() == Some(&'[') {
                    chars.next();
                    for next in chars.by_ref() {
                        if next.is_ascii_alphabetic() {
                            break;
                        }
                    }
                }
            }
            '\r' => {}
            _ => out.push(ch),
        }
    }
    out
}

/// Every byte from the dump lines in `text`, in order.
pub fn dump_bytes(text: &str) -> Vec<u8> {
    let mut out = Vec::new();
    for line in text.lines() {
        let Some((addr, rest)) = line.trim_start().split_once(": ") else {
            continue;
        };
        let Some(star) = rest.find('*') else {
            continue;
        };
        if addr.is_empty() || !addr.chars().all(|ch| ch.is_ascii_hexdigit()) {
            continue;
        }
        let bytes: Option<Vec<u8>> = rest[..star]
            .replace('-', " ")
            .split_whitespace()
            .map(|pair| u8::from_str_radix(pair, 16).ok())
            .collect();
        out.extend(bytes.unwrap_or_default());
    }
    out
}
