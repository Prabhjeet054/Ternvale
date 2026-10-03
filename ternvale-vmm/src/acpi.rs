//! ACPI tables in the reserved guest window ([`ACPI_BASE`], [`ACPI_SIZE`]),
//! and their hand-off to EDK2 over fw_cfg.
//!
//! `ternvale-acpi` builds the tables from [`crate::acpi_check::acpi_config`];
//! this module maps the window into the guest read-only, copies them in,
//! reads them back by walking guest memory from the RSDP, and fails the boot
//! if what landed disagrees with `platform.rs` ([`crate::acpi_check::check`]).
//!
//! With `firmware_tables = "acpi"`, [`prepare`] also turns the checked
//! tables into QEMU's linker/loader files on a [`FwCfg`] device. EDK2
//! ArmVirtQemu installs ACPI from those files and stops publishing the DTB to
//! the OS (see `ternvale_acpi::LoaderBlobs`). The OS reads EDK2's copies at
//! addresses EDK2 picks, not this window. With `"fdt"` nothing is mapped or
//! offered, and EDK2 publishes the DTB as before.

use ternvale_acpi::{DumpedTable, LoaderBlobs, TableRef};
use ternvale_config::{FirmwareTables, VmConfig};

use crate::fw_cfg::{FwCfg, FW_CFG_REG_SIZE};
use crate::machine::MachineError;
use crate::memory::GuestMemory;
use crate::mmio::MmioDevice;
use crate::platform::{ACPI_BASE, ACPI_SIZE, FW_CFG_BASE};

/// A device for the MMIO bus: base, size, model.
pub(crate) type BusDevice = (u64, u64, Box<dyn MmioDevice>);

/// The firmware-table setup for `config`. `acpi` (firmware boots only):
/// [`install`] the window, then return the fw_cfg device carrying the loader
/// files built from the tables read back. `fdt`: nothing.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(name = %config.name, cpus = config.cpus))]
pub(crate) fn prepare(
    memory: &mut GuestMemory,
    vm: &ternvale_hv::Vm,
    config: &VmConfig,
) -> Result<Option<BusDevice>, MachineError> {
    let tables = config.effective_firmware_tables();
    if tables == FirmwareTables::Fdt || config.firmware.is_none() {
        tracing::info!(target: "ternvale::acpi", firmware_tables = %tables, "device tree only; acpi tables not installed");
        return Ok(None);
    }
    let checked = install(memory, vm, config.cpus)?;
    let device = fw_cfg_for(&checked)?;
    tracing::info!(
        target: "ternvale::acpi",
        firmware_tables = %tables,
        fw_cfg = %format!("{FW_CFG_BASE:#x}"),
        tables = checked.len(),
        "acpi tables offered to EDK2 over fw_cfg"
    );
    Ok(Some((FW_CFG_BASE, FW_CFG_REG_SIZE, Box::new(device))))
}

/// The fw_cfg device holding the loader files for `tables`.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(tables = tables.len()))]
pub(crate) fn fw_cfg_for(tables: &[DumpedTable]) -> Result<FwCfg, MachineError> {
    let refs: Vec<TableRef<'_>> = tables.iter().map(TableRef::from).collect();
    let blobs = LoaderBlobs::build(&refs)?;
    Ok(FwCfg::new(blobs.files()?)?)
}

/// Map the ACPI window into `vm` (guest read-only), write the tables for
/// `cpus` vCPUs, check them against the platform map, and return them as
/// read back from guest memory.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::acpi",
    skip_all,
    fields(base = %format!("{ACPI_BASE:#x}"), size = %format!("{ACPI_SIZE:#x}"), cpus)
)]
pub(crate) fn install(
    memory: &mut GuestMemory,
    vm: &ternvale_hv::Vm,
    cpus: u32,
) -> Result<Vec<DumpedTable>, MachineError> {
    memory.map_flags(vm, ACPI_BASE, ACPI_SIZE, ternvale_hv::HV_MEMORY_READ)?;
    write_tables(memory, cpus)?;
    let tables = dump_tables(memory)?;
    crate::acpi_check::check(&tables, cpus)?;
    Ok(tables)
}

/// Build the tables for [`ACPI_BASE`] and `cpus` vCPUs and write them into
/// `memory`, which must already hold the ACPI window.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(cpus))]
pub(crate) fn write_tables(
    memory: &mut GuestMemory,
    cpus: u32,
) -> Result<ternvale_acpi::AcpiTables, MachineError> {
    let config = crate::acpi_check::acpi_config(cpus);
    let tables = ternvale_acpi::AcpiTables::build(ACPI_BASE, ACPI_SIZE, &config)?;
    tables.write(|gpa, bytes| memory.write_bytes(gpa, bytes))?;
    tracing::debug!(
        target: "ternvale::acpi",
        rsdp = %format!("{:#x}", tables.rsdp_gpa()),
        used = tables.used_bytes(),
        "acpi window filled"
    );
    Ok(tables)
}

/// Every table reachable from the RSDP at [`ACPI_BASE`], read from guest memory.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all)]
pub(crate) fn dump_tables(memory: &GuestMemory) -> Result<Vec<DumpedTable>, MachineError> {
    let tables = ternvale_acpi::walk(ACPI_BASE, |gpa, len| {
        let mut bytes = vec![0; len];
        memory.read_bytes(gpa, &mut bytes).map(|()| bytes)
    })?;
    tracing::info!(
        target: "ternvale::acpi",
        rsdp = %format!("{ACPI_BASE:#x}"),
        tables = tables.len(),
        signatures = %tables.iter().map(|t| t.signature.trim_end()).collect::<Vec<_>>().join(","),
        "acpi tables read back from guest memory"
    );
    Ok(tables)
}

/// Create a VM, map, fill, and check the ACPI window exactly as a boot with
/// `cpus` vCPUs does, and read the tables back out of guest memory. Needs the
/// hypervisor entitlement.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(cpus))]
pub fn dump_guest_acpi(cpus: u32) -> Result<Vec<DumpedTable>, MachineError> {
    let vm = ternvale_hv::Vm::create()?;
    let mut memory = GuestMemory::new()?;
    let tables = install(&mut memory, &vm, cpus)?;
    drop(memory);
    drop(vm);
    Ok(tables)
}

/// Like [`dump_guest_acpi`] without a VM: write the tables for `cpus` vCPUs
/// into a host buffer standing in for the ACPI window, walk them back from
/// the RSDP at [`ACPI_BASE`], and run the same platform check. Same bytes and
/// addresses, no hypervisor entitlement; `scripts/acpi-check.sh` uses it.
#[tracing::instrument(level = "debug", target = "ternvale::acpi", skip_all, fields(cpus))]
pub fn build_acpi_offline(cpus: u32) -> Result<Vec<DumpedTable>, MachineError> {
    let config = crate::acpi_check::acpi_config(cpus);
    let built = ternvale_acpi::AcpiTables::build(ACPI_BASE, ACPI_SIZE, &config)?;
    let mut window = vec![0u8; ACPI_SIZE as usize];
    let span = |gpa: u64, len: usize| {
        let start = usize::try_from(gpa.checked_sub(ACPI_BASE)?).ok()?;
        let end = start.checked_add(len)?;
        (end <= ACPI_SIZE as usize).then_some(start..end)
    };
    built.write(|gpa, bytes| {
        let range = span(gpa, bytes.len()).ok_or("outside the acpi window")?;
        window[range].copy_from_slice(bytes);
        Ok::<(), &str>(())
    })?;
    let tables = ternvale_acpi::walk(ACPI_BASE, |gpa, len| {
        span(gpa, len)
            .map(|range| window[range].to_vec())
            .ok_or("outside the acpi window")
    })?;
    crate::acpi_check::check(&tables, cpus)?;
    tracing::info!(
        target: "ternvale::acpi",
        cpus,
        tables = tables.len(),
        signatures = %tables.iter().map(|t| t.signature.trim_end()).collect::<Vec<_>>().join(","),
        "acpi tables built offline and read back"
    );
    Ok(tables)
}

#[cfg(test)]
#[path = "acpi_hv_test.rs"]
mod hv_test;

#[cfg(test)]
mod tests {
    use super::*;

    fn read(memory: &GuestMemory, gpa: u64, len: usize) -> Vec<u8> {
        let mut out = vec![0; len];
        memory.read_bytes(gpa, &mut out).expect("read");
        out
    }

    fn u64_at(bytes: &[u8], at: usize) -> u64 {
        u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
    }

    #[test]
    fn tables_land_in_the_acpi_window_and_chain_from_the_rsdp() {
        let mut memory = GuestMemory::new().expect("memory");
        memory.add_region(ACPI_BASE, ACPI_SIZE).expect("region");
        let tables = write_tables(&mut memory, 1).expect("write");
        assert_eq!(tables.rsdp_gpa(), ACPI_BASE);

        let rsdp = read(&memory, ACPI_BASE, ternvale_acpi::RSDP_LEN);
        assert_eq!(&rsdp[..8], b"RSD PTR ");
        assert!(ternvale_acpi::rsdp_checksums_ok(
            rsdp.as_slice().try_into().unwrap()
        ));
        let xsdt_gpa = u64_at(&rsdp, ternvale_acpi::RSDP_XSDT_OFFSET);
        let xsdt = read(&memory, xsdt_gpa, ternvale_acpi::xsdt_len(6));
        assert_eq!(&xsdt[..4], b"XSDT");
        assert_eq!(ternvale_acpi::byte_sum(&xsdt), 0);
        let fadt_gpa = u64_at(&xsdt, ternvale_acpi::SDT_HEADER_LEN);
        let fadt = read(&memory, fadt_gpa, ternvale_acpi::FADT_LEN);
        assert_eq!(&fadt[..4], b"FACP");
        assert_eq!(ternvale_acpi::byte_sum(&fadt), 0);
        let flags = u32::from_le_bytes(
            fadt[ternvale_acpi::FLAGS_OFFSET..ternvale_acpi::FLAGS_OFFSET + 4]
                .try_into()
                .unwrap(),
        );
        assert_ne!(flags & ternvale_acpi::HW_REDUCED_ACPI, 0);
        let dsdt_gpa = u64_at(&fadt, ternvale_acpi::X_DSDT_OFFSET);
        let header = read(&memory, dsdt_gpa, ternvale_acpi::SDT_HEADER_LEN);
        let header = ternvale_acpi::SdtHeader::parse(&header).expect("dsdt header");
        assert_eq!(&header.signature, b"DSDT");
        let dsdt = read(&memory, dsdt_gpa, header.length as usize);
        assert_eq!(ternvale_acpi::byte_sum(&dsdt), 0);
        assert!(dsdt_gpa + u64::from(header.length) <= ACPI_BASE + ACPI_SIZE);
    }

    #[test]
    fn dump_reads_back_exactly_what_was_written() {
        let mut memory = GuestMemory::new().expect("memory");
        memory.add_region(ACPI_BASE, ACPI_SIZE).expect("region");
        let written = write_tables(&mut memory, 4).expect("write");
        let dumped = dump_tables(&memory).expect("dump");
        assert_eq!(dumped.len(), written.tables().len());
        for (dumped, table) in dumped.iter().zip(written.tables()) {
            assert_eq!(dumped.signature, table.signature);
            assert_eq!(dumped.gpa, table.gpa);
            assert_eq!(dumped.bytes, table.bytes);
            assert!(dumped.checksum_ok(), "{}", dumped.signature);
        }
    }

    #[test]
    fn read_back_tables_become_the_three_fw_cfg_loader_files() {
        let mut memory = GuestMemory::new().expect("memory");
        memory.add_region(ACPI_BASE, ACPI_SIZE).expect("region");
        write_tables(&mut memory, 2).expect("write");
        let dumped = dump_tables(&memory).expect("dump");
        let device = fw_cfg_for(&dumped).expect("fw_cfg");
        let names: Vec<&str> = device.file_names().collect();
        assert_eq!(
            names,
            [
                ternvale_acpi::RSDP_FILE,
                ternvale_acpi::TABLES_FILE,
                ternvale_acpi::LOADER_FILE
            ]
        );
    }

    #[test]
    fn offline_build_matches_the_guest_memory_read_back() {
        for cpus in [1, 4, 123] {
            let mut memory = GuestMemory::new().expect("memory");
            memory.add_region(ACPI_BASE, ACPI_SIZE).expect("region");
            write_tables(&mut memory, cpus).expect("write");
            let guest = dump_tables(&memory).expect("dump");
            let offline = build_acpi_offline(cpus).expect("offline");
            assert_eq!(offline, guest, "cpus={cpus}");
        }
        assert!(build_acpi_offline(0).is_err());
    }

    #[test]
    fn missing_window_is_a_named_write_error() {
        let mut memory = GuestMemory::new().expect("memory");
        let error = write_tables(&mut memory, 1).unwrap_err();
        let text = error.to_string();
        assert!(text.contains("RSD PTR"), "{text}");
        assert!(text.contains(&format!("{ACPI_BASE:#x}")), "{text}");
    }
}
