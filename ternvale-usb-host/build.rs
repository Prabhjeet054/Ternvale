fn main() {
    let sdk = std::process::Command::new("xcrun")
        .args(["--show-sdk-path"])
        .output()
        .expect("xcrun --show-sdk-path");
    let sdk_string = String::from_utf8(sdk.stdout).expect("sdk path utf8");
    let sdk_path = sdk_string.trim();

    cc::Build::new()
        .file("src/usb_capture.m")
        .include("include")
        .flag("-fobjc-arc")
        .flag("-Wno-deprecated-declarations")
        .flag(format!("-isysroot{}", sdk_path))
        .compile("ternvale_usb_capture");

    println!("cargo:rustc-link-lib=framework=IOUSBHost");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=IOKit");
}
