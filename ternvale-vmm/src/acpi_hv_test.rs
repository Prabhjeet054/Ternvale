use super::*;

#[test]
#[ignore = "needs-hv"]
fn tables_read_back_from_a_real_vm_match_the_builder() {
    let dumped = dump_guest_acpi().expect("dump guest acpi");
    let built = AcpiTables::build(ACPI_BASE, ACPI_SIZE, PsciConduit::Hvc).expect("build");
    let names: Vec<_> = dumped.iter().map(|t| t.signature.as_str()).collect();
    assert_eq!(names, ["RSD PTR ", "XSDT", "FACP", "DSDT"]);
    for (dumped, table) in dumped.iter().zip(built.tables()) {
        assert_eq!(dumped.gpa, table.gpa, "{}", table.signature);
        assert_eq!(dumped.bytes, table.bytes, "{}", table.signature);
        assert!(dumped.checksum_ok(), "{}", table.signature);
    }
}
