//! Compile the TvVz Swift static library and link Virtualization.framework.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=Sources/TvVz");
    println!("cargo:rerun-if-changed=Package.swift");
    println!("cargo:rerun-if-changed=include/tv_vz.h");

    let manifest = PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR"));
    let lib = out.join("libtv_vz.a");
    let sources: Vec<PathBuf> = [
        "Sources/TvVz/Log.swift",
        "Sources/TvVz/Host.swift",
        "Sources/TvVz/Socket.swift",
        "Sources/TvVz/Bridge.swift",
        "Sources/TvVz/Macos.swift",
        "Sources/TvVz/MacosDownload.swift",
        "Sources/TvVz/Devices.swift",
        "Sources/TvVz/View.swift",
    ]
    .into_iter()
    .map(|p| manifest.join(p))
    .collect();

    let sdk = xcrun(&["--show-sdk-path"]);
    let mut cmd = Command::new("xcrun");
    cmd.args(["swiftc", "-emit-library", "-static", "-parse-as-library"])
        .args(["-module-name", "TvVz"])
        .args(["-swift-version", "5"])
        .args(["-target", "arm64-apple-macos13.0"])
        .args(["-sdk", sdk.trim()])
        .args([
            "-framework",
            "Virtualization",
            "-framework",
            "Foundation",
            "-framework",
            "AppKit",
            "-framework",
            "CryptoKit",
        ])
        .args(["-Xfrontend", "-disable-implicit-concurrency-module-import"])
        .arg("-O")
        .arg("-o")
        .arg(&lib);
    for src in &sources {
        cmd.arg(src);
    }
    let status = cmd.status().expect("spawn swiftc");
    if !status.success() {
        panic!("swiftc failed with {status}; install Xcode CLT and Virtualization.framework");
    }

    println!("cargo:rustc-link-search=native={}", out.display());
    println!("cargo:rustc-link-lib=static=tv_vz");
    println!("cargo:rustc-link-arg=-Wl,-force_load,{}", lib.display());
    println!("cargo:rustc-link-lib=framework=Virtualization");
    println!("cargo:rustc-link-lib=framework=Foundation");
    println!("cargo:rustc-link-lib=framework=AppKit");
    println!("cargo:rustc-link-lib=framework=CryptoKit");
    link_swift_runtime();
}

fn xcrun(args: &[&str]) -> String {
    let out = Command::new("xcrun").args(args).output().expect("xcrun");
    if !out.status.success() {
        panic!(
            "xcrun {:?} failed: {}",
            args,
            String::from_utf8_lossy(&out.stderr)
        );
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn link_swift_runtime() {
    let toolchain = PathBuf::from(xcrun(&["--find", "swiftc"]));
    let usr = toolchain
        .parent()
        .and_then(Path::parent)
        .unwrap_or_else(|| Path::new("/usr"));
    let swift_libs = usr.join("lib/swift/macosx");
    if swift_libs.is_dir() {
        println!("cargo:rustc-link-search=native={}", swift_libs.display());
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", swift_libs.display());
    }
    println!("cargo:rustc-link-search=native=/usr/lib/swift");
    println!("cargo:rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
    let concurrency = usr.join("lib/swift-5.5/macosx");
    if concurrency.is_dir() {
        println!("cargo:rustc-link-search=native={}", concurrency.display());
        println!("cargo:rustc-link-arg=-Wl,-rpath,{}", concurrency.display());
    }
    for lib in [
        "swiftCore",
        "swift_Concurrency",
        "swiftDispatch",
        "swiftFoundation",
        "swiftObjectiveC",
        "swiftSwiftOnoneSupport",
        "swiftDarwin",
        "swiftIOKit",
        "swiftXPC",
        "swiftos",
        "swiftsimd",
        "swiftCompatibilityConcurrency",
    ] {
        println!("cargo:rustc-link-lib=dylib={lib}");
    }
}
