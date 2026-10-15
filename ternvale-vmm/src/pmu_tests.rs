use super::*;

#[test]
fn no_pmu_becomes_pmuv3_and_other_fields_survive() {
    // Hypervisor.framework's default on an M-series host (macOS 27).
    assert_eq!(with_pmuv3(0x1030_5006), 0x1030_5106);
    assert_eq!(with_pmuv3(0), 0x100);
    assert_eq!(with_pmuv3(u64::MAX & !PMUVER_MASK), u64::MAX & !0xe00);
}

#[test]
fn impdef_pmu_becomes_pmuv3() {
    assert_eq!(with_pmuv3(0xf06), 0x106);
}

#[test]
fn an_architected_pmu_is_kept() {
    for version in [0x1u64, 0x4, 0x5, 0x6, 0x7, 0x8] {
        let dfr0 = 0x1030_5006 | (version << 8);
        assert_eq!(with_pmuv3(dfr0), dfr0, "PMUVer {version:#x}");
    }
}
