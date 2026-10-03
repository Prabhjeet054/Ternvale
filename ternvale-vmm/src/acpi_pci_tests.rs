//! DSDT `\_SB.PCI0` vs the DTB `pcie@3f000000` node and the PCI model.
//!
//! The descriptors are written out here from the DTB cells (ACPI 6.5
//! §6.4.3.5.2 and §6.4.3.5.3), not with `ternvale_acpi::aml_resource`, so a
//! change to the DTB, the platform constants, or the AML encoder alone fails.

use ternvale_acpi::{prt_routes, spi_gsiv, verify, SDT_HEADER_LEN};

use super::dtb_tests::{dtb, table, written};
use super::*;
use crate::fdt::read::{node, Node};
use crate::pci::swizzle;

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// DWord memory descriptor: fixed, positive decode, non-cacheable, read/write.
fn dword_memory(consumer: bool, base: u32, len: u32) -> Vec<u8> {
    let flags = if consumer { 0x0d } else { 0x0c };
    let mut out = vec![0x87, 23, 0, 0, flags, 0x01];
    for field in [0, base, base + (len - 1), 0, len] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out
}

/// Word bus-number descriptor: producer, fixed, positive decode.
fn word_bus_number(first: u16, last: u16) -> Vec<u8> {
    let mut out = vec![0x88, 13, 0, 2, 0x0c, 0];
    for field in [0, first, last, 0, last - first + 1] {
        out.extend_from_slice(&field.to_le_bytes());
    }
    out
}

fn pcie(nodes: &[Node]) -> &Node {
    node(nodes, &format!("pcie@{PCIE_ECAM_BASE:x}"))
}

fn cells(node: &Node, name: &str) -> Vec<u32> {
    node.cells(name)
        .unwrap_or_else(|| panic!("{} has no {name}", node.path))
}

#[test]
fn pci0_crs_and_res0_match_the_dtb_ranges_bus_range_and_reg() {
    let (_, tables) = written(1);
    let dsdt = &table(&tables, "DSDT").bytes[SDT_HEADER_LEN..];
    let nodes = dtb(1);
    let pcie = pcie(&nodes);

    let [space, pci_hi, pci_lo, cpu_hi, cpu_lo, size_hi, size_lo] = cells(pcie, "ranges")[..]
    else {
        panic!("one ranges entry expected");
    };
    assert_eq!(space, 0x0200_0000, "32-bit non-prefetchable memory");
    assert_eq!(
        (pci_hi, pci_lo),
        (cpu_hi, cpu_lo),
        "identity mapped, so _TRA is 0"
    );
    assert_eq!(
        (cpu_hi, size_hi),
        (0, 0),
        "below 4 GiB, so a DWord descriptor"
    );
    assert!(
        contains(dsdt, &dword_memory(false, cpu_lo, size_lo)),
        "PCI0 _CRS lacks the DTB window {cpu_lo:#x}+{size_lo:#x}"
    );

    let [first, last] = cells(pcie, "bus-range")[..] else {
        panic!("bus-range is two cells");
    };
    assert!(
        contains(dsdt, &word_bus_number(first as u16, last as u16)),
        "PCI0 _CRS lacks buses {first}..={last}"
    );

    let [reg_hi, reg_lo, len_hi, len_lo] = cells(pcie, "reg")[..] else {
        panic!("reg is one 2+2 cell entry");
    };
    assert_eq!((reg_hi, len_hi), (0, 0));
    assert!(
        contains(dsdt, &dword_memory(true, reg_lo, len_lo)),
        "RES0 does not reserve the DTB ECAM {reg_lo:#x}+{len_lo:#x}"
    );
}

#[test]
fn prt_matches_the_dtb_interrupt_map_and_the_pci_swizzle() {
    let routes = prt_routes(&acpi_config(1));
    assert_eq!(routes.len(), 32 * 4);
    let nodes = dtb(1);
    let pcie = pcie(&nodes);
    let [mask, 0, 0, pin_mask] = cells(pcie, "interrupt-map-mask")[..] else {
        panic!("interrupt-map-mask");
    };
    let mut matched = 0;
    for entry in cells(pcie, "interrupt-map").chunks_exact(10) {
        let (unit, pin, spi) = (entry[0], entry[3], entry[8]);
        for route in routes.iter().filter(|r| {
            (u32::from(r.device) << 11) & mask == unit && (u32::from(r.pin) + 1) & pin_mask == pin
        }) {
            assert_eq!(
                route.gsiv,
                spi_gsiv(spi),
                "device {} pin {}: _PRT GSIV vs DTB SPI {spi}",
                route.device,
                route.pin
            );
            matched += 1;
        }
    }
    assert_eq!(
        matched,
        routes.len(),
        "every _PRT entry has a DTB interrupt-map entry"
    );
    for route in &routes {
        let line = swizzle(route.device, route.pin + 1) as u32;
        assert_eq!(route.gsiv, spi_gsiv(PCI_INTX_SPI0 + line), "{route:?}");
    }
}

#[test]
fn a_pci_window_or_intx_edit_on_one_side_is_drift() {
    let (_, tables) = written(1);
    let layout = Layout::virt(0);
    let mut window = acpi_config(1);
    window.pci.mmio_len = 0x1000_0000;
    let problems = platform_problems(&window, &layout).join("\n");
    assert!(
        problems.contains("DSDT PCI0 _CRS: platform pcie-mmio is 0x10000000+0x2f000000"),
        "{problems}"
    );
    let mut intx = acpi_config(1);
    intx.pci.intx_spis[0] = 7;
    let problems = platform_problems(&intx, &layout).join("\n");
    assert!(problems.contains("DSDT PCI0 _PRT"), "{problems}");
    match verify(&tables, &intx) {
        Err(AcpiError::Drift { problems }) => {
            assert!(
                problems.iter().any(|p| p.contains("DSDT: AML differs")),
                "{problems:?}"
            );
        }
        other => panic!("expected DSDT drift, got {other:?}"),
    }
    check(&tables, 1).expect("the written tables still pass");
}
