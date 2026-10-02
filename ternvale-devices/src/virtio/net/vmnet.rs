//! Stub vmnet.framework backend (shared/NAT mode). Built with `--features vmnet`.
//!
//! The real backend will call `vmnet_start_interface` with
//! `vmnet_operation_mode_key = VMNET_SHARED_MODE`, then move frames with
//! `vmnet_read` / `vmnet_write`. That API takes Objective-C blocks and a dispatch
//! queue, and the project keeps raw FFI in `ternvale-hv`, so the FFI and a safe
//! wrapper belong there. Until then [`VmnetBackend::open_shared`] fails.
//!
//! Starting a vmnet interface needs root, or the restricted
//! `com.apple.vm.networking` entitlement that Apple grants on request.
//! See `docs/NET.md`.

use super::backend::NetBackend;
use super::NetError;

/// `operating_modes_t` value for NAT shared with the host.
// TODO(verify): value 1001 is from <vmnet/vmnet.h> (HOST=1000, SHARED=1001, BRIDGED=1002);
// confirm against the SDK header when the FFI lands in ternvale-hv.
pub const VMNET_SHARED_MODE: u32 = 1001;

/// Placeholder for a vmnet.framework shared-mode interface.
#[derive(Debug)]
pub struct VmnetBackend {
    _private: (),
}

impl VmnetBackend {
    /// Start a shared-mode interface. Always fails in this stub.
    #[tracing::instrument(level = "debug", target = "ternvale::net")]
    pub fn open_shared() -> Result<Self, NetError> {
        tracing::warn!(
            target: "ternvale::net",
            mode = VMNET_SHARED_MODE,
            "vmnet backend is a stub; use the loopback backend"
        );
        Err(NetError::Unsupported {
            backend: "vmnet",
            reason: "stub: vmnet.framework FFI is not implemented yet (needs root or the \
                     com.apple.vm.networking entitlement)",
        })
    }
}

impl NetBackend for VmnetBackend {
    fn name(&self) -> &'static str {
        "vmnet"
    }

    fn send(&mut self, frame: &[u8]) -> Result<(), NetError> {
        tracing::warn!(target: "ternvale::net", len = frame.len(), "vmnet stub dropped tx frame");
        Err(NetError::Unsupported {
            backend: "vmnet",
            reason: "stub backend cannot send",
        })
    }

    fn recv(&mut self) -> Result<Option<Vec<u8>>, NetError> {
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::super::open_backend;
    use super::{VmnetBackend, VMNET_SHARED_MODE};

    #[test]
    fn stub_reports_unsupported() {
        assert_eq!(VMNET_SHARED_MODE, 1001);
        let err = VmnetBackend::open_shared().unwrap_err();
        assert!(err.to_string().contains("stub"), "{err}");
        let err = open_backend("vmnet").err().expect("vmnet stub fails");
        assert!(err.to_string().contains("vmnet"), "{err}");
    }
}
