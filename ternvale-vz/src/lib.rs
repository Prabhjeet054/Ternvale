//! Virtualization.framework Linux host.
//!
//! Swift lives in `Sources/TvVz` and is built as a static library. The bridge
//! is a small **C ABI** (`include/tv_vz.h`), not swift-bridge: we already own
//! the Swift, need explicit hopping onto the VM `DispatchQueue`, and must not
//! pull a codegen crate into the workspace.
//!
//! Apple requires every `VZVirtualMachine` method on the queue passed to
//! `init(configuration:queue:)` (Virtualization.framework, macOS 12). The
//! Swift side uses that queue; Rust never calls into VZ except through the C
//! ABI. The process needs `com.apple.security.virtualization` (not
//! `com.apple.security.hypervisor`). TODO(verify): Apple's current
//! entitlement name against `codesign -d --entitlements -` on a signed binary.

mod error;
mod ffi;
mod host;
mod macos;
mod macos_host;
mod plan;

pub use error::VzError;
pub use host::VzHost;
pub use macos::{MacosIdentity, AUX_NAME, IDENTITY_NAME};
pub use macos_host::{
    create_aux, download, inspect, install, write_platform, DownloadResult, MacosRestoreInfo,
};
pub use plan::VzPlan;

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-vz");
    }
}
