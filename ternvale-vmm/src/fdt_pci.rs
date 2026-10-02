//! `pcie@3f000000`: the generic ECAM host bridge node.
//!
//! Same shape as QEMU virt's node: one non-prefetchable 32-bit memory range
//! mapped 1:1, and an `interrupt-map` for devices 0..4 x INTA..INTD masked by
//! `0x1800` (device bits 11..12), so every other device number reuses the
//! same swizzle. There is no `msi-parent`: MSI-X is a stub, so Linux and EDK2
//! fall back to INTx.

use vm_fdt::{FdtWriter, FdtWriterResult};

use crate::pci::{swizzle, ECAM_BUSES, PCI_INTX_LINES, PCI_INTX_SPI0};
use crate::platform::{PCIE_ECAM_BASE, PCIE_ECAM_SIZE, PCIE_MMIO_BASE, PCIE_MMIO_SIZE};

/// `phys.hi` space code for 32-bit non-prefetchable memory.
const SPACE_MEM32: u32 = 0x0200_0000;
/// GIC interrupt type cell for an SPI.
const GIC_SPI: u32 = 0;
/// GIC flags cell: level-high, as INTx is level-triggered.
const LEVEL_HIGH: u32 = 4;

pub(super) fn pcie(w: &mut FdtWriter, gic: u32) -> FdtWriterResult<()> {
    let node = w.begin_node(&format!("pcie@{PCIE_ECAM_BASE:x}"))?;
    w.property_string("compatible", "pci-host-ecam-generic")?;
    w.property_string("device_type", "pci")?;
    w.property_array_u32("reg", &super::reg(PCIE_ECAM_BASE, PCIE_ECAM_SIZE))?;
    w.property_array_u32("bus-range", &[0, u32::from(ECAM_BUSES) - 1])?;
    w.property_u32("#address-cells", 3)?;
    w.property_u32("#size-cells", 2)?;
    w.property_u32("#interrupt-cells", 1)?;
    w.property_u32("linux,pci-domain", 0)?;
    w.property_null("dma-coherent")?;
    w.property_array_u32("ranges", &ranges())?;
    w.property_array_u32("interrupt-map-mask", &[0x1800, 0, 0, 7])?;
    w.property_array_u32("interrupt-map", &interrupt_map(gic))?;
    w.end_node(node)?;
    tracing::debug!(
        target: "ternvale::boot",
        ecam = format!("{PCIE_ECAM_BASE:#x}"),
        window = format!("{PCIE_MMIO_BASE:#x}"),
        window_size = format!("{PCIE_MMIO_SIZE:#x}"),
        intx_spi0 = PCI_INTX_SPI0,
        "dtb pcie node"
    );
    Ok(())
}

/// `<phys.hi phys.mid phys.lo  cpu.hi cpu.lo  size.hi size.lo>`, identity mapped.
fn ranges() -> [u32; 7] {
    let (base_hi, base_lo) = ((PCIE_MMIO_BASE >> 32) as u32, PCIE_MMIO_BASE as u32);
    let (size_hi, size_lo) = ((PCIE_MMIO_SIZE >> 32) as u32, PCIE_MMIO_SIZE as u32);
    [
        SPACE_MEM32,
        base_hi,
        base_lo,
        base_hi,
        base_lo,
        size_hi,
        size_lo,
    ]
}

/// Ten cells per entry: child unit address (3), child pin (1), GIC phandle (1),
/// GIC unit address (2, the intc node's `#address-cells`), GIC specifier (3).
fn interrupt_map(gic: u32) -> Vec<u32> {
    let mut cells = Vec::with_capacity(PCI_INTX_LINES * PCI_INTX_LINES * 10);
    for device in 0..PCI_INTX_LINES as u8 {
        for pin in 1..=PCI_INTX_LINES as u8 {
            let spi = PCI_INTX_SPI0 + swizzle(device, pin) as u32;
            cells.extend_from_slice(&[
                u32::from(device) << 11,
                0,
                0,
                u32::from(pin),
                gic,
                0,
                0,
                GIC_SPI,
                spi,
                LEVEL_HIGH,
            ]);
        }
    }
    cells
}

#[cfg(test)]
mod tests {
    #[test]
    fn interrupt_map_swizzles_device_and_pin() {
        let map = super::interrupt_map(2);
        assert_eq!(map.len(), 160);
        let entry = |i: usize| &map[i * 10..i * 10 + 10];
        assert_eq!(
            entry(0),
            &[0, 0, 0, 1, 2, 0, 0, 0, 3, 4],
            "00:00 INTA -> SPI 3"
        );
        assert_eq!(entry(4)[0], 1 << 11, "device 1 unit address");
        assert_eq!(entry(4)[8], 4, "00:01 INTA -> SPI 4");
        assert_eq!(entry(7)[8], 3, "00:01 INTD wraps to SPI 3");
        assert_eq!(entry(15)[8], 5, "00:03 INTD -> SPI 5");
    }

    #[test]
    fn ranges_map_the_window_one_to_one() {
        assert_eq!(
            super::ranges(),
            [0x0200_0000, 0, 0x1000_0000, 0, 0x1000_0000, 0, 0x2f00_0000]
        );
    }
}
