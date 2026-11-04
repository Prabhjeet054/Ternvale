//! vmnet operating-mode constants, named host-only UUIDs, bridgeable NIC list.

#[link(name = "tv_vmnet_shim", kind = "static")]
unsafe extern "C" {
    fn tv_vmnet_list_bridgeable(out: *mut i8, cap: u32) -> i32;
    fn tv_vmnet_network_uuid(name: *const i8, out: *mut u8);
}

/// `VMNET_HOST_MODE` from `<vmnet/vmnet.h>`.
pub const VMNET_HOST_MODE: u32 = 1000;
/// `VMNET_SHARED_MODE` from `<vmnet/vmnet.h>`.
pub const VMNET_SHARED_MODE: u32 = 1001;
/// `VMNET_BRIDGED_MODE` from `<vmnet/vmnet.h>`.
pub const VMNET_BRIDGED_MODE: u32 = 1002;

pub(crate) fn c_string(s: &str) -> std::ffi::CString {
    let cleaned: Vec<u8> = s.as_bytes().iter().copied().filter(|b| *b != 0).collect();
    // SAFETY: interior NUL bytes were stripped.
    unsafe { std::ffi::CString::from_vec_unchecked(cleaned) }
}

fn c_err(buf: &[i8]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// UUID v5-style identifier for a named host-only LAN.
#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
pub fn network_uuid(name: &str) -> [u8; 16] {
    let c = c_string(name);
    let mut out = [0u8; 16];
    // SAFETY: `c` is a C string; `out` is 16 bytes.
    unsafe { tv_vmnet_network_uuid(c.as_ptr(), out.as_mut_ptr()) };
    out
}

/// Names from `vmnet_copy_shared_interface_list` (bridgeable physical NICs).
#[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
pub fn list_bridgeable() -> Vec<String> {
    let mut buf = vec![0i8; 4096];
    // SAFETY: writable buffer for comma-separated names.
    let n = unsafe { tv_vmnet_list_bridgeable(buf.as_mut_ptr(), buf.len() as u32) };
    if n <= 0 {
        tracing::debug!(target: "ternvale::net::vmnet", count = n, "no bridgeable interfaces");
        return Vec::new();
    }
    let text = c_err(&buf);
    let names: Vec<String> = text
        .split(',')
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .collect();
    tracing::info!(
        target: "ternvale::net::vmnet",
        count = names.len(),
        "enumerated bridgeable interfaces"
    );
    names
}

#[cfg(test)]
mod tests {
    use super::{network_uuid, VMNET_BRIDGED_MODE, VMNET_HOST_MODE, VMNET_SHARED_MODE};

    #[test]
    fn modes_match_sdk() {
        assert_eq!(VMNET_HOST_MODE, 1000);
        assert_eq!(VMNET_SHARED_MODE, 1001);
        assert_eq!(VMNET_BRIDGED_MODE, 1002);
    }

    #[test]
    fn host_only_uuid_is_stable() {
        let a = network_uuid("lab");
        let b = network_uuid("lab");
        let c = network_uuid("other");
        assert_eq!(a, b);
        assert_ne!(a, c);
        assert_eq!(network_uuid(""), network_uuid("default"));
    }
}
