use super::*;

#[test]
fn pc_in_the_vector_table_names_the_entry() {
    let vbar = 0x4000_3000;
    assert_eq!(
        vector_name(vbar, vbar),
        Some("synchronous, current EL, SP_EL0")
    );
    // Windows' cdboot.efi parked at VBAR + 0x200 with `b .`.
    assert_eq!(
        vector_name(vbar, vbar + 0x200),
        Some("synchronous, current EL, SP_ELx")
    );
    assert_eq!(
        vector_name(vbar, vbar + 0x2fc),
        Some("irq, current EL, SP_ELx")
    );
    assert_eq!(
        vector_name(vbar, vbar + 0x7fc),
        Some("serror, lower EL, AArch32")
    );
}

#[test]
fn pc_outside_the_table_is_not_a_vector() {
    let vbar = 0x4000_3000;
    assert_eq!(vector_name(vbar, vbar - 4), None);
    assert_eq!(vector_name(vbar, vbar + 0x800), None);
    assert_eq!(vector_name(0, u64::MAX), None);
    // VBAR_EL1 bits 10:0 are RES0; stray low bits do not shift the table.
    assert_eq!(
        vector_name(vbar | 0x7ff, vbar + 0x200),
        Some("synchronous, current EL, SP_ELx")
    );
}

#[test]
fn exception_classes_are_named() {
    assert_eq!(exception_class(0x9600_0010), "data abort, same EL");
    assert_eq!(exception_class(0x8600_0006), "instruction abort, same EL");
    assert_eq!(
        exception_class(0x6200_0000),
        "msr/mrs/system instruction trap"
    );
    assert_eq!(exception_class(0x0200_0000), "unknown reason");
    assert_eq!(exception_class(0xf200_0000), "brk instruction");
    assert_eq!(exception_class(0x1fe0_0000), "sve/simd/fp access trap");
    assert_eq!(exception_class(0xfc00_0000), "other");
}
