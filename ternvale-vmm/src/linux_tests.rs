use std::cell::Cell;

use super::*;
use crate::memory::HOST_PAGE_SIZE;
use crate::platform::RAM_BASE;

struct FakeCpu {
    x: [Cell<u64>; 4],
    pc: Cell<u64>,
    cpsr: Cell<u64>,
}

impl FakeCpu {
    fn new() -> Self {
        Self {
            x: [Cell::new(1), Cell::new(1), Cell::new(1), Cell::new(1)],
            pc: Cell::new(1),
            cpsr: Cell::new(1),
        }
    }
}

impl BootRegs for FakeCpu {
    fn set_gpr(&self, index: u8, value: u64) -> Result<(), LinuxBootError> {
        self.x[usize::from(index)].set(value);
        Ok(())
    }

    fn set_pc(&self, value: u64) -> Result<(), LinuxBootError> {
        self.pc.set(value);
        Ok(())
    }

    fn set_cpsr(&self, value: u64) -> Result<(), LinuxBootError> {
        self.cpsr.set(value);
        Ok(())
    }
}

fn image(text_offset: u64, image_size: u64, flags: u64, body: usize) -> Vec<u8> {
    let mut bytes = vec![0u8; HEADER_LEN + body];
    bytes[TEXT_OFFSET_AT..TEXT_OFFSET_AT + 8].copy_from_slice(&text_offset.to_le_bytes());
    bytes[IMAGE_SIZE_AT..IMAGE_SIZE_AT + 8].copy_from_slice(&image_size.to_le_bytes());
    bytes[FLAGS_AT..FLAGS_AT + 8].copy_from_slice(&flags.to_le_bytes());
    bytes[MAGIC_AT..MAGIC_AT + 4].copy_from_slice(&IMAGE_MAGIC.to_le_bytes());
    bytes
}

#[test]
fn rejects_a_bad_magic() {
    let mut bytes = image(0, 64, 0, 0);
    bytes[MAGIC_AT] = 0;
    let error = parse_header(&bytes).unwrap_err();
    assert!(
        matches!(error, LinuxBootError::BadMagic { magic: 0x644d_5200 }),
        "{error}"
    );
}

#[test]
fn places_the_kernel_at_text_offset_above_a_2mib_base() {
    let header = parse_header(&image(0x80000, 0x200000, 0, 0)).expect("header");
    let layout = place(
        RAM_BASE,
        16 * 1024 * 1024,
        &header,
        HEADER_LEN as u64,
        100,
        32,
    )
    .expect("place");
    assert_eq!(layout.kernel, RAM_BASE + 0x80000);
    assert_eq!(layout.kernel % KERNEL_ALIGN, 0x80000);
    assert_eq!(layout.kernel_bytes, 0x200000);
    assert!(layout.dtb >= layout.kernel + layout.kernel_bytes);
    assert_eq!(layout.dtb % 8, 0);
    assert!(layout.initrd >= layout.dtb + layout.dtb_bytes);
    assert!(layout.initrd + layout.initrd_bytes <= RAM_BASE + 16 * 1024 * 1024);
    assert!(layout.kernel + layout.kernel_bytes <= layout.dtb);
    assert!(layout.dtb + layout.dtb_bytes <= layout.initrd);
}

#[test]
fn load_writes_images_and_boot_registers() {
    let kernel = image(0, 128, 0, 8);
    let initrd = b"initrd-bytes";
    let dtb = b"dtb-bytes!!";
    let mut memory = GuestMemory::new().expect("pages");
    memory.add_region(RAM_BASE, HOST_PAGE_SIZE).expect("region");
    let cpu = FakeCpu::new();
    let layout = load_linux(
        &mut memory,
        &cpu,
        RAM_BASE,
        HOST_PAGE_SIZE,
        &kernel,
        initrd,
        dtb,
    )
    .expect("load");
    assert_eq!(layout.kernel, RAM_BASE);
    assert_eq!(cpu.x[0].get(), layout.dtb);
    assert_eq!(cpu.x[1].get(), 0);
    assert_eq!(cpu.x[2].get(), 0);
    assert_eq!(cpu.x[3].get(), 0);
    assert_eq!(cpu.pc.get(), layout.kernel);
    assert_eq!(cpu.cpsr.get(), CPSR_EL1H_MASKED);
    let mut got = vec![0u8; kernel.len()];
    memory.read_bytes(layout.kernel, &mut got).expect("kernel");
    assert_eq!(got, kernel);
    let mut got = vec![0u8; dtb.len()];
    memory.read_bytes(layout.dtb, &mut got).expect("dtb");
    assert_eq!(got, dtb);
}

#[test]
fn rejects_a_kernel_that_does_not_fit() {
    let header = parse_header(&image(0, 0x2000, 0, 0)).expect("header");
    let error = place(RAM_BASE, 0x1000, &header, HEADER_LEN as u64, 0, 16).unwrap_err();
    assert!(
        matches!(
            error,
            LinuxBootError::NoFit {
                reason: "kernel does not fit",
                ..
            }
        ),
        "{error}"
    );
}

#[test]
fn rejects_zero_image_size() {
    let error = parse_header(&image(0x80000, 0, 0, 0)).unwrap_err();
    assert!(matches!(error, LinuxBootError::ZeroImageSize), "{error}");
}

#[test]
fn kernel_initrd_and_dtb_do_not_overlap() {
    let header = parse_header(&image(0, 0x1000, 0, 0)).expect("header");
    let layout = place(RAM_BASE, 0x8000, &header, 0x1000, 0x100, 0x20).expect("place");
    let kernel = (layout.kernel, layout.kernel + layout.kernel_bytes);
    let dtb = (layout.dtb, layout.dtb + layout.dtb_bytes);
    let initrd = (layout.initrd, layout.initrd + layout.initrd_bytes);
    assert!(kernel.1 <= dtb.0, "kernel overlaps dtb");
    assert!(dtb.1 <= initrd.0, "dtb overlaps initrd");
    assert!(initrd.1 <= RAM_BASE + 0x8000);
}

#[test]
fn rejects_an_initrd_that_collides_with_the_kernel_reservation() {
    let header = parse_header(&image(0, 0x3000, 0, 0)).expect("header");
    let error = place(RAM_BASE, 0x3100, &header, 0x100, 0x1000, 0x10).unwrap_err();
    assert!(
        matches!(
            error,
            LinuxBootError::NoFit {
                reason: "initrd does not fit",
                ..
            }
        ),
        "{error}"
    );
}

#[test]
#[ignore = "needs-test-assets"]
fn logs_the_entry_address_of_the_fetched_image() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../test-assets");
    let kernel = std::fs::read(root.join("Image")).expect("run scripts/fetch-test-kernel.sh");
    let initrd = std::fs::read(root.join("initramfs.cpio")).expect("initramfs");
    let header = parse_header(&kernel).expect("header");
    let entry = RAM_BASE + header.text_offset;
    let ram = 128 * 1024 * 1024;
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-linux", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let guard =
        ternvale_log::init(ternvale_log::LogConfig::new("linux", dir.clone())).expect("log");
    let mut memory = GuestMemory::new().expect("pages");
    memory.add_region(RAM_BASE, ram).expect("region");
    let cpu = FakeCpu::new();
    let layout =
        load_linux(&mut memory, &cpu, RAM_BASE, ram, &kernel, &initrd, b"dtb").expect("load");
    assert_eq!(layout.kernel, entry);
    assert_eq!(cpu.pc.get(), entry);
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    let kernel_field = format!("kernel=\"{entry:#x}\"");
    let pc_field = format!("pc=\"{entry:#x}\"");
    assert!(text.contains(&kernel_field), "{text}");
    assert!(text.contains(&pc_field), "{text}");
    std::fs::remove_dir_all(&dir).expect("remove log dir");
}
