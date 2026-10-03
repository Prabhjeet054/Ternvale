//! ACPI tables vs the DTB blob, field by field. The tables are decoded from
//! guest memory after `write_tables`; the DTB is read from the bytes
//! `build_fdt` returns (`fdt::read`), not from `dtc` text or shared constants,
//! so an edit to either description alone fails here.

use ternvale_acpi::{decode_gtdt, decode_madt, decode_mcfg, GtdtInfo, TIMER_ALWAYS_ON};

use super::*;
use crate::acpi::{dump_tables, write_tables};
use crate::fdt::read::{node, nodes, Node};
use crate::fdt::{build_fdt, GuestFdt};
use crate::memory::GuestMemory;
use crate::platform::{PCIE_ECAM_SIZE, RAM_BASE};
use ternvale_acpi::{checksum, SDT_CHECKSUM_OFFSET};

/// The timer PPIs Step 14 put in the DTB (QEMU virt `gic-version=3`): secure
/// physical, non-secure physical, virtual, hypervisor. Changing the timer
/// interrupts means changing this on purpose, after checking the guest side.
const STEP14_TIMER_PPIS: [u32; 4] = [13, 14, 11, 10];
/// DT interrupt specifier type for a PPI.
const DT_PPI: u32 = 1;
/// DT interrupt specifier flags: level-high.
const DT_LEVEL_HIGH: u32 = 4;

fn written(cpus: u32) -> (GuestMemory, Vec<DumpedTable>) {
    let mut memory = GuestMemory::new().expect("memory");
    memory.add_region(ACPI_BASE, ACPI_SIZE).expect("region");
    write_tables(&mut memory, cpus).expect("write");
    let tables = dump_tables(&memory).expect("dump");
    (memory, tables)
}

fn table<'a>(tables: &'a [DumpedTable], signature: &str) -> &'a DumpedTable {
    tables
        .iter()
        .find(|t| t.signature == signature)
        .unwrap_or_else(|| panic!("no {signature}"))
}

fn dtb(cpus: u32) -> Vec<Node> {
    let fdt = GuestFdt {
        bootargs: String::new(),
        ram_base: RAM_BASE,
        ram_size: 256 << 20,
        initrd_start: RAM_BASE,
        initrd_end: RAM_BASE,
        cpu_count: cpus,
        firmware: true,
    };
    nodes(&build_fdt(&fdt).expect("dtb"))
}

/// Every way the DTB `timer` node (`interrupts` cells, `always-on`) and a
/// decoded GTDT disagree.
fn timer_disagreements(cells: &[u32], always_on: bool, gtdt: &GtdtInfo) -> Vec<String> {
    let mut problems = Vec::new();
    if cells.len() != 12 {
        problems.push(format!("DTB timer has {} cells, want 4 x 3", cells.len()));
        return problems;
    }
    for (slot, spec) in cells.chunks_exact(3).enumerate() {
        let (kind, ppi, flags) = (spec[0], spec[1], spec[2]);
        if kind != DT_PPI {
            problems.push(format!("timer {slot}: DTB type {kind}, not a PPI"));
        }
        if gtdt.gsivs[slot] != 16 + ppi {
            problems.push(format!(
                "timer {slot}: DTB PPI {ppi} is GSIV {}, GTDT says {}",
                16 + ppi,
                gtdt.gsivs[slot]
            ));
        }
        let gtdt_level_high = gtdt.flags[slot] & 0b11 == 0;
        if (flags == DT_LEVEL_HIGH) != gtdt_level_high {
            problems.push(format!(
                "timer {slot}: DTB flags {flags}, GTDT flags {:#x}",
                gtdt.flags[slot]
            ));
        }
        let gtdt_always_on = gtdt.flags[slot] & TIMER_ALWAYS_ON != 0;
        if always_on != gtdt_always_on {
            problems.push(format!(
                "timer {slot}: DTB always-on {always_on}, GTDT always-on {gtdt_always_on}"
            ));
        }
    }
    problems
}

#[test]
fn madt_gicc_count_matches_the_configured_vcpu_count() {
    for cpus in (1..=16).chain([64, 123]) {
        let (_, tables) = written(cpus);
        let madt = decode_madt(&table(&tables, "APIC").bytes).expect("madt");
        assert_eq!(madt.giccs.len(), cpus as usize, "{cpus} vcpus");
        for (index, gicc) in madt.giccs.iter().enumerate() {
            assert_eq!(gicc.uid, index as u32, "{cpus} vcpus");
            assert_eq!(gicc.flags & ternvale_acpi::GICC_ENABLED, 1, "cpu {index}");
            assert_eq!(gicc.mpidr, u64::from(crate::smp::dt_cpu_reg(index as u32)));
        }
        if cpus <= 16 {
            let all = dtb(cpus);
            let regs: Vec<u64> = all
                .iter()
                .filter(|n| n.path.starts_with("/cpus/cpu@"))
                .map(|n| u64::from(n.cells("reg").expect("cpu reg")[0]))
                .collect();
            let mut mpidrs: Vec<u64> = madt.giccs.iter().map(|g| g.mpidr).collect();
            mpidrs.sort_unstable();
            let mut regs = regs;
            regs.sort_unstable();
            assert_eq!(mpidrs, regs, "{cpus} vcpus: MADT MPIDRs vs DTB cpu regs");
        }
        check(&tables, cpus).unwrap_or_else(|e| panic!("{cpus} vcpus: {e}"));
    }
}

#[test]
fn more_vcpus_than_redistributors_fail_the_boot_check() {
    let (_, tables) = written(124);
    let error = check(&tables, 124).unwrap_err().to_string();
    assert!(error.contains("124 cpus need 0xf80000 bytes"), "{error}");
}

#[test]
fn gtdt_ppis_equal_the_dtb_timer_cells() {
    let (_, tables) = written(1);
    let gtdt = decode_gtdt(&table(&tables, "GTDT").bytes).expect("gtdt");
    let all = dtb(1);
    let timer = node(&all, "timer");
    let cells = timer.cells("interrupts").expect("timer interrupts");
    let problems = timer_disagreements(&cells, timer.has("always-on"), &gtdt);
    assert!(
        problems.is_empty(),
        "DTB and GTDT disagree:\n{}",
        problems.join("\n")
    );

    let dtb_ppis: Vec<u32> = cells.chunks_exact(3).map(|spec| spec[1]).collect();
    assert_eq!(
        dtb_ppis, STEP14_TIMER_PPIS,
        "DTB timer PPIs changed; update STEP14_TIMER_PPIS only after checking the guest"
    );
    assert_eq!(gtdt.gsivs, STEP14_TIMER_PPIS.map(|ppi| ppi + 16));
}

#[test]
fn a_timer_edit_on_one_side_only_is_caught() {
    let (mut memory, tables) = written(1);
    let gtdt = decode_gtdt(&table(&tables, "GTDT").bytes).expect("gtdt");
    let all = dtb(1);
    let timer = node(&all, "timer");
    let cells = timer.cells("interrupts").expect("timer interrupts");

    let mut dtb_edit = cells.clone();
    dtb_edit[4] = 12;
    let text = timer_disagreements(&dtb_edit, true, &gtdt).join("\n");
    assert!(
        text.contains("timer 1: DTB PPI 12 is GSIV 28, GTDT says 30"),
        "{text}"
    );
    let mut edge = cells.clone();
    edge[2] = 1;
    let text = timer_disagreements(&edge, true, &gtdt).join("\n");
    assert!(text.contains("timer 0: DTB flags 1"), "{text}");
    let text = timer_disagreements(&cells, false, &gtdt).join("\n");
    assert_eq!(text.matches("DTB always-on false").count(), 4, "{text}");

    // GTDT edited (non-secure EL1 GSIV 30 -> 28), checksum fixed: a builder bug.
    let at = table(&tables, "GTDT").gpa;
    let mut bytes = table(&tables, "GTDT").bytes.clone();
    bytes[56..60].copy_from_slice(&28u32.to_le_bytes());
    bytes[SDT_CHECKSUM_OFFSET] = 0;
    bytes[SDT_CHECKSUM_OFFSET] = checksum(&bytes);
    memory.write_bytes(at, &bytes).expect("tamper");
    let tables = dump_tables(&memory).expect("dump");
    let edited = decode_gtdt(&table(&tables, "GTDT").bytes).expect("gtdt");
    let text = timer_disagreements(&cells, true, &edited).join("\n");
    assert!(
        text.contains("timer 1: DTB PPI 14 is GSIV 30, GTDT says 28"),
        "{text}"
    );
    let error = check(&tables, 1).unwrap_err().to_string();
    assert!(error.contains("GTDT timer GSIVs"), "{error}");
}

#[test]
fn mcfg_base_and_size_equal_the_ecam_window() {
    let (_, tables) = written(1);
    let allocations = decode_mcfg(&table(&tables, "MCFG").bytes).expect("mcfg");
    let [ecam] = allocations[..] else {
        panic!("one allocation expected: {allocations:?}")
    };
    let size = (u64::from(ecam.end_bus) - u64::from(ecam.start_bus) + 1) << 20;
    let base = ecam.base + (u64::from(ecam.start_bus) << 20);
    assert_eq!(
        (base, size, ecam.segment),
        (PCIE_ECAM_BASE, PCIE_ECAM_SIZE, 0)
    );

    let layout = Layout::virt(0);
    let region = layout
        .regions()
        .iter()
        .find(|r| r.name == "pcie-ecam")
        .expect("pcie-ecam region");
    assert_eq!((region.base, region.size), (base, size));

    let all = dtb(1);
    let pcie = node(&all, &format!("pcie@{base:x}"));
    let reg = pcie.cells("reg").expect("pcie reg");
    let split = |v: u64| [(v >> 32) as u32, v as u32];
    assert_eq!(reg, [split(base), split(size)].concat(), "DTB pcie reg");
    assert_eq!(
        pcie.cells("bus-range").expect("bus-range"),
        vec![u32::from(ecam.start_bus), u32::from(ecam.end_bus)]
    );
}
