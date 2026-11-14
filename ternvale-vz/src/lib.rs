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
mod rosetta;
mod tvzs;

pub use error::VzError;
pub use ffi::{nsapp_run, nsapp_stop};
pub use host::VzHost;
pub use macos::{parse_hex, MacosIdentity, AUX_NAME, IDENTITY_NAME};
pub use macos_host::{
    create_aux, download, inspect, install, new_machine_identifier, write_platform, DownloadResult,
    MacosRestoreInfo,
};
pub use plan::VzPlan;
pub use rosetta::{
    availability as rosetta_availability, install_with_consent as install_rosetta,
    require_installed as require_rosetta_installed, user_consented as rosetta_user_consented,
    Availability as RosettaAvailability, AOT_ABSTRACT_NAME as ROSETTA_AOT_NAME, BINFMT_CREDENTIALS,
    BINFMT_FIX_BINARY, BINFMT_MAGIC, BINFMT_MASK, BINFMT_PRESERVE, FS_TAG as ROSETTA_FS_TAG,
    GUEST_MOUNT as ROSETTA_GUEST_MOUNT, GUEST_RUNTIME as ROSETTA_RUNTIME,
};
pub use tvzs::{
    unwrap_to as unwrap_tvzs, wrap as wrap_tvzs, MAGIC as TVZS_MAGIC, VERSION as TVZS_VERSION,
};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-vz");
    }
}
