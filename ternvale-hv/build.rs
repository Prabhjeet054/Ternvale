fn main() {
    println!("cargo:rerun-if-changed=src/vmnet_shim.c");
    if std::env::var_os("CARGO_FEATURE_VMNET").is_none() {
        return;
    }
    cc::Build::new()
        .file("src/vmnet_shim.c")
        .flag("-fblocks")
        .flag("-Wno-unused-parameter")
        .compile("tv_vmnet_shim");
    println!("cargo:rustc-link-lib=framework=vmnet");
    println!("cargo:rustc-link-lib=framework=System");
    println!("cargo:rustc-link-lib=framework=CoreFoundation");
}
