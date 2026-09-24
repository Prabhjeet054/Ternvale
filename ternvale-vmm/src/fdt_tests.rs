use super::*;
use crate::memory::HOST_PAGE_SIZE;
use crate::platform::{
    GIC_DIST_BASE, GIC_DIST_SIZE, GIC_REDIST_BASE, GIC_REDIST_SIZE, RAM_BASE, UART_BASE,
    VIRTIO_MMIO_BASE, VIRTIO_MMIO_SLOTS, VIRTIO_MMIO_SLOT_SIZE,
};

fn sample() -> GuestFdt {
    GuestFdt {
        bootargs: "console=ttyAMA0".to_string(),
        ram_base: RAM_BASE,
        ram_size: 256 * 1024 * 1024,
        initrd_start: RAM_BASE + 0x0200_0000,
        initrd_end: RAM_BASE + 0x0200_1000,
        cpu_count: 2,
    }
}

#[test]
fn blob_contains_the_platform_nodes() {
    let blob = build_fdt(&sample()).expect("dtb");
    assert_eq!(&blob[..4], &[0xd0, 0x0d, 0xfe, 0xed]);
    let text = String::from_utf8_lossy(&blob);
    for needle in [
        "linux,dummy-virt",
        "bootargs",
        "pl011@9000000",
        "arm,pl011",
        "arm,gic-v3",
        "arm,psci-1.0",
        "arm,armv8-timer",
        "fixed-clock",
        "virtio,mmio",
        "enable-method",
        "psci",
    ] {
        assert!(text.contains(needle), "missing {needle}");
    }
}

#[test]
fn write_copies_the_blob_and_logs_its_size() {
    let blob = build_fdt(&sample()).expect("dtb");
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-fdt", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let guard = ternvale_log::init(ternvale_log::LogConfig::new("fdt", dir.clone())).expect("log");
    let mut memory = GuestMemory::new().expect("pages");
    memory.add_region(RAM_BASE, HOST_PAGE_SIZE).expect("region");
    write_fdt(&mut memory, RAM_BASE, &blob, Some(&dir)).expect("write");
    let mut got = vec![0u8; blob.len()];
    memory.read_bytes(RAM_BASE, &mut got).expect("read");
    assert_eq!(got, blob);
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    assert!(text.contains(&format!("bytes={}", blob.len())), "{text}");
    std::fs::remove_dir_all(&dir).expect("remove");
}

#[test]
fn dumps_a_decompiled_dtb_when_asked() {
    let blob = build_fdt(&sample()).expect("dtb");
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-fdtdump", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    // SAFETY: this test owns the process env for the duration of the call and
    // restores it before returning. `TERNVALE_DUMP_DTB` is not read elsewhere
    // in this process except `write_fdt`.
    unsafe { std::env::set_var("TERNVALE_DUMP_DTB", "1") };
    let mut memory = GuestMemory::new().expect("pages");
    memory.add_region(RAM_BASE, HOST_PAGE_SIZE).expect("region");
    write_fdt(&mut memory, RAM_BASE, &blob, Some(&dir)).expect("write");
    unsafe { std::env::remove_var("TERNVALE_DUMP_DTB") };
    let saved = std::fs::read(dir.join("guest.dtb")).expect("guest.dtb");
    assert_eq!(saved, blob);
    let dts = dir.join("guest.dts");
    if dts.exists() {
        let text = std::fs::read_to_string(&dts).expect("dts");
        assert!(text.contains("pl011@9000000"), "{text}");
        assert!(text.contains("arm,gic-v3"), "{text}");
    }
    std::fs::remove_dir_all(&dir).expect("remove");
}

fn dtc_dts(blob: &[u8]) -> String {
    let dir = std::env::temp_dir().join(format!(
        "ternvale-vmm-{}-dtc-{}",
        std::process::id(),
        blob.len()
    ));
    std::fs::create_dir_all(&dir).expect("dtc dir");
    let dtb = dir.join("guest.dtb");
    std::fs::write(&dtb, blob).expect("write dtb");
    let output = std::process::Command::new("dtc")
        .args(["-I", "dtb", "-O", "dts"])
        .arg(&dtb)
        .output()
        .expect("dtc is on PATH");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.trim().is_empty(), "dtc warnings:\n{stderr}");
    std::fs::remove_dir_all(&dir).expect("remove");
    String::from_utf8(output.stdout).expect("dts utf8")
}

#[test]
fn decompiled_dts_matches_platform_regs() {
    let fdt = sample();
    let dts = dtc_dts(&build_fdt(&fdt).expect("dtb"));
    let memory = format!("reg = <0x00 {:#x} 0x00 {:#x}>", fdt.ram_base, fdt.ram_size);
    assert!(dts.contains("memory@40000000"), "{dts}");
    assert!(dts.contains(&memory), "{dts}");
    let gic = format!(
        "reg = <0x00 {:#x} 0x00 {:#x} 0x00 {:#x} 0x00 {:#x}>",
        GIC_DIST_BASE, GIC_DIST_SIZE, GIC_REDIST_BASE, GIC_REDIST_SIZE
    );
    assert!(dts.contains("intc@8000000"), "{dts}");
    assert!(dts.contains(&gic), "{dts}");
    let uart = format!("reg = <0x00 {:#x} 0x00 0x1000>", UART_BASE);
    assert!(dts.contains("pl011@9000000"), "{dts}");
    assert!(dts.contains(&uart), "{dts}");
    let first = format!(
        "reg = <0x00 {:#x} 0x00 {:#x}>",
        VIRTIO_MMIO_BASE, VIRTIO_MMIO_SLOT_SIZE
    );
    assert!(dts.contains("virtio_mmio@a000000"), "{dts}");
    assert!(dts.contains(&first), "{dts}");
    let last_base = VIRTIO_MMIO_BASE + (VIRTIO_MMIO_SLOTS - 1) * VIRTIO_MMIO_SLOT_SIZE;
    assert!(dts.contains(&format!("virtio_mmio@{last_base:x}")), "{dts}");
    assert_eq!(
        dts.matches("virtio_mmio@").count(),
        VIRTIO_MMIO_SLOTS as usize
    );
    assert!(dts.contains("method = \"hvc\""), "{dts}");
    assert!(dts.contains("enable-method = \"psci\""), "{dts}");
    assert!(dts.contains("stdout-path = \"/pl011@9000000\""), "{dts}");
    assert!(dts.contains("#address-cells = <0x02>"), "{dts}");
    assert!(dts.contains("#size-cells = <0x02>"), "{dts}");
}

#[test]
fn rejects_zero_cpus() {
    let mut fdt = sample();
    fdt.cpu_count = 0;
    let error = build_fdt(&fdt).unwrap_err();
    assert!(matches!(error, FdtError::NoCpus), "{error}");
}
