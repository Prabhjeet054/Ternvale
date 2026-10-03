use super::*;
use crate::acpi::{dump_tables, write_tables};
use crate::fdt::{build_fdt, tests::dtc_dts, GuestFdt};
use crate::memory::GuestMemory;
use crate::platform::{GIC_REDIST_SIZE, PCIE_ECAM_SIZE, RAM_BASE};

/// The ACPI window written for `cpus` vCPUs, in memory without a VM.
fn written(cpus: u32) -> (GuestMemory, Vec<DumpedTable>) {
    let mut memory = GuestMemory::new().expect("memory");
    memory.add_region(ACPI_BASE, ACPI_SIZE).expect("region");
    write_tables(&mut memory, cpus).expect("write");
    let tables = dump_tables(&memory).expect("dump");
    (memory, tables)
}

fn drift(error: MachineError) -> Vec<String> {
    match error {
        MachineError::Acpi(AcpiError::Drift { problems }) => problems,
        other => panic!("expected drift, got {other}"),
    }
}

#[test]
fn config_comes_from_the_platform_constants() {
    let config = acpi_config(3);
    config.validate().expect("valid");
    assert_eq!(config.mpidrs, vec![0, 1, 2]);
    let wide = acpi_config(17);
    assert_eq!(wide.mpidrs[16], 0x100, "cpu 16 is Aff1 1, Aff0 0");
    for (cpu, mpidr) in wide.mpidrs.iter().enumerate() {
        assert_eq!(*mpidr, u64::from(crate::smp::dt_cpu_reg(cpu as u32)));
    }
    assert_eq!(
        (config.gic.dist_base, config.gic.redist_base),
        (0x0800_0000, 0x080a_0000)
    );
    assert_eq!(u64::from(config.gic.redist_len), GIC_REDIST_SIZE);
    assert_eq!(config.timer.ppis, [13, 14, 11, 10]);
    assert_eq!((config.ecam.base, config.ecam.end_bus), (0x3f00_0000, 15));
    assert_eq!(
        (config.uart.base, config.uart.len, config.uart.spi),
        (0x0900_0000, 0x1000, 1)
    );
    assert!(platform_problems(&config, &Layout::virt(0)).is_empty());
}

#[test]
fn written_tables_pass_the_check_for_any_cpu_count() {
    for cpus in [1, 2, 8, 16, 64] {
        let (_, tables) = written(cpus);
        check(&tables, cpus).unwrap_or_else(|e| panic!("{cpus} cpus: {e}"));
    }
}

#[test]
fn tampered_guest_memory_fails_and_names_the_field() {
    let (mut memory, tables) = written(2);
    let madt = tables.iter().find(|t| t.signature == "APIC").expect("apic");
    let gicd_base = madt.gpa + 36 + 8 + 2 * 82 + 8;
    memory
        .write_bytes(gicd_base, &0x0801_0000u64.to_le_bytes())
        .expect("tamper");
    let tables = dump_tables(&memory).expect("dump");
    let problems = drift(check(&tables, 2).unwrap_err());
    let text = problems.join("\n");
    assert!(text.contains("APIC GICD (base, version)"), "{text}");
    assert!(text.contains("8010000"), "{text}");
    assert!(text.contains("APIC: checksum"), "{text}");
}

#[test]
fn tables_for_another_cpu_count_are_drift() {
    let (_, tables) = written(2);
    let text = drift(check(&tables, 3).unwrap_err()).join("\n");
    assert!(
        text.contains("APIC GICC count: expected 3, found 2"),
        "{text}"
    );
}

#[test]
fn config_that_leaves_the_platform_map_is_named() {
    let layout = Layout::virt(0);
    let mut config = acpi_config(1);
    config.gic.dist_base = 0x0801_0000;
    config.gic.redist_len = 0x2_0000;
    config.ecam.end_bus = 31;
    config.uart.base = 0x0900_3800;
    let text = platform_problems(&config, &layout).join("\n");
    assert!(
        text.contains("GICD: platform gic-dist is 0x8000000+0x10000, tables say 0x8010000"),
        "{text}"
    );
    assert!(text.contains("GICR: platform gic-redist"), "{text}");
    assert!(text.contains("MCFG: platform pcie-ecam"), "{text}");
    assert!(text.contains("SPCR/DBG2 UART: platform uart"), "{text}");

    let many = acpi_config(200);
    let text = platform_problems(&many, &layout).join("\n");
    assert!(text.contains("200 cpus need 0x1900000 bytes"), "{text}");

    let mut irqs = acpi_config(1);
    irqs.timer.ppis[3] = 12;
    irqs.uart.spi = 5;
    let text = platform_problems(&irqs, &layout).join("\n");
    assert!(
        text.contains("GTDT: tables use timer PPIs [13, 14, 11, 12] always-on true, DTB timer node uses [13, 14, 11, 10]"),
        "{text}"
    );
    assert!(text.contains("SPCR/DBG2: tables use uart SPI 5"), "{text}");
}

#[test]
fn a_table_outside_the_window_is_drift() {
    let (_, mut tables) = written(1);
    tables[4].gpa = ACPI_BASE + ACPI_SIZE - 8;
    let text = drift(check(&tables, 1).unwrap_err()).join("\n");
    assert!(text.contains("outside the acpi window"), "{text}");
}

/// The text of DTS node `name` (`name {` up to its first `};`).
fn node<'a>(dts: &'a str, name: &str) -> &'a str {
    let start = dts
        .find(&format!("{name} {{"))
        .unwrap_or_else(|| panic!("no {name} in\n{dts}"));
    let len = dts[start..].find("};").expect("node end");
    &dts[start..start + len]
}

/// Both boot paths describe one machine: every address and interrupt decoded
/// from the ACPI tables in guest memory appears in the DTB for the same guest.
#[test]
fn dtb_and_acpi_agree() {
    let cpus = 4;
    let (_, tables) = written(cpus);
    let bytes = |s: &str| &tables.iter().find(|t| t.signature == s).expect(s).bytes;
    let madt = ternvale_acpi::decode_madt(bytes("APIC")).expect("madt");
    let gtdt = ternvale_acpi::decode_gtdt(bytes("GTDT")).expect("gtdt");
    let mcfg = ternvale_acpi::decode_mcfg(bytes("MCFG")).expect("mcfg");
    let spcr = ternvale_acpi::decode_spcr(bytes("SPCR")).expect("spcr");
    let dbg2 = ternvale_acpi::decode_dbg2(bytes("DBG2")).expect("dbg2");
    let fdt = GuestFdt {
        bootargs: String::new(),
        ram_base: RAM_BASE,
        ram_size: 256 << 20,
        initrd_start: RAM_BASE,
        initrd_end: RAM_BASE,
        cpu_count: cpus,
        firmware: true,
    };
    let dts = dtc_dts(&build_fdt(&fdt).expect("dtb"));

    let timer = node(&dts, "timer");
    let cells: Vec<String> = gtdt
        .gsivs
        .iter()
        .map(|gsiv| format!("0x01 {:#04x} 0x04", gsiv - 16))
        .collect();
    let interrupts = format!("interrupts = <{}>", cells.join(" "));
    assert!(timer.contains(&interrupts), "{interrupts}\n{timer}");
    let always_on = gtdt
        .flags
        .iter()
        .all(|f| f & ternvale_acpi::TIMER_ALWAYS_ON != 0);
    assert_eq!(timer.contains("always-on"), always_on, "{timer}");

    let [(gicd, 3)] = madt.gicds[..] else {
        panic!("{:?}", madt.gicds)
    };
    let [(gicr, gicr_len)] = madt.gicrs[..] else {
        panic!("{:?}", madt.gicrs)
    };
    let intc = node(&dts, &format!("intc@{gicd:x}"));
    assert!(intc.contains("arm,gic-v3"), "{intc}");
    let redist = format!("0x00 {gicr:#x} 0x00 {gicr_len:#x}>");
    assert!(intc.contains(&format!("reg = <0x00 {gicd:#x} ")), "{intc}");
    assert!(intc.contains(&redist), "{redist}\n{intc}");

    assert_eq!(
        madt.giccs.len(),
        dts.matches("device_type = \"cpu\"").count()
    );
    for gicc in &madt.giccs {
        let reg = gicc.mpidr & 0xff_ffff;
        let cpu = node(&dts, &format!("cpu@{reg:x}"));
        assert!(cpu.contains(&format!("reg = <{reg:#04x}>")), "{cpu}");
    }

    let [ecam] = mcfg[..] else { panic!("{mcfg:?}") };
    let pcie = node(&dts, &format!("pcie@{:x}", ecam.base));
    let size = (u64::from(ecam.end_bus - ecam.start_bus) + 1) << 20;
    assert_eq!(size, PCIE_ECAM_SIZE);
    assert!(
        pcie.contains(&format!("reg = <0x00 {:#x} 0x00 {size:#x}>", ecam.base)),
        "{pcie}"
    );
    let range = format!(
        "bus-range = <{:#04x} {:#04x}>",
        ecam.start_bus, ecam.end_bus
    );
    assert!(pcie.contains(&range), "{range}\n{pcie}");

    let [debug] = dbg2[..] else {
        panic!("{dbg2:?}")
    };
    assert_eq!(debug.base, spcr.base);
    let uart = node(&dts, &format!("pl011@{:x}", spcr.base.address));
    let reg = format!(
        "reg = <0x00 {:#x} 0x00 {:#x}>",
        spcr.base.address, debug.size
    );
    assert!(uart.contains(&reg), "{reg}\n{uart}");
    let irq = format!("interrupts = <0x00 {:#04x} 0x04>", spcr.gsiv - 32);
    assert!(uart.contains(&irq), "{irq}\n{uart}");
}
