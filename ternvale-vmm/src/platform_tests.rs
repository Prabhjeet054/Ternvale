use super::{Layout, PlatformError, Region, RAM_BASE};

fn mib(n: u64) -> u64 {
    n * 1024 * 1024
}

#[test]
fn default_layout_is_aligned_and_does_not_overlap() {
    let layout = Layout::virt(mib(256));
    layout.validate().expect("default");
    for region in layout.regions() {
        assert_eq!(region.base % 0x4000, 0, "{}", region.name);
        assert_eq!(region.size % 0x4000, 0, "{}", region.name);
    }
    assert_eq!(layout.regions().len(), 9);
    assert!(layout
        .regions()
        .iter()
        .any(|region| region.name == "ram" && region.base == RAM_BASE));
}

#[test]
fn ram_sizes_stay_above_the_device_windows() {
    for ram in [mib(256), mib(1024), mib(4096)] {
        let layout = Layout::virt(ram);
        layout
            .validate()
            .unwrap_or_else(|error| panic!("{ram:#x}: {error}"));
        let ram_region = layout
            .regions()
            .iter()
            .find(|region| region.name == "ram")
            .unwrap();
        for other in layout.regions() {
            if other.name == "ram" {
                continue;
            }
            assert!(other.base + other.size <= ram_region.base, "{}", other.name);
        }
    }
}

#[test]
fn overlapping_layout_is_rejected() {
    let layout = Layout {
        regions: vec![
            Region {
                name: "a",
                base: 0,
                size: 0x8000,
            },
            Region {
                name: "b",
                base: 0x4000,
                size: 0x8000,
            },
        ],
    };
    let error = layout.validate().unwrap_err();
    assert!(matches!(error, PlatformError::Overlap { .. }), "{error}");
    assert!(error.to_string().contains("overlaps"), "{error}");
}

#[test]
fn misaligned_region_is_rejected() {
    let layout = Layout {
        regions: vec![Region {
            name: "uart",
            base: 0x0900_0000,
            size: 0x1000,
        }],
    };
    let error = layout.validate().unwrap_err();
    assert!(matches!(error, PlatformError::Misaligned { .. }), "{error}");
}

#[test]
fn dump_logs_every_region() {
    let dir = std::env::temp_dir().join(format!("ternvale-vmm-{}-map", std::process::id()));
    std::fs::create_dir_all(&dir).expect("log dir");
    let guard = ternvale_log::init(ternvale_log::LogConfig::new("map", dir.clone())).expect("log");
    let layout = Layout::virt(mib(256));
    layout.dump();
    let path = guard.log_path().to_path_buf();
    drop(guard);
    let text = std::fs::read_to_string(&path).expect("log");
    assert!(text.contains("guest physical map"), "{text}");
    for name in [
        "flash",
        "gic-dist",
        "gic-redist",
        "uart",
        "rtc",
        "virtio-mmio",
        "pcie-mmio",
        "pcie-ecam",
        "ram",
    ] {
        assert!(text.contains(name), "{name} missing in {text}");
    }
    assert!(text.contains("0x40000000"), "{text}");
    assert!(text.contains("0x9000000"), "{text}");
    std::fs::remove_dir_all(&dir).expect("remove log dir");
}
