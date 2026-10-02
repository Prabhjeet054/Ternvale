//! Host entropy for virtio-rng through `getentropy(2)`.
//!
//! Not part of Hypervisor.framework, but raw FFI is kept in this crate.

use std::ffi::c_void;

use crate::error::HvError;
use crate::ffi::getentropy;

/// Largest buffer one `getentropy` call accepts (`GETENTROPY_MAX` in the man page).
pub const GETENTROPY_MAX: usize = 256;

/// Fill `buf` with random bytes from the kernel CSPRNG, 256 bytes per call.
#[tracing::instrument(level = "debug", target = "ternvale::hv", skip_all, fields(len = buf.len()))]
pub fn fill_entropy(buf: &mut [u8]) -> Result<(), HvError> {
    for chunk in buf.chunks_mut(GETENTROPY_MAX) {
        // SAFETY: `chunk` is a live, writable slice of `chunk.len()` bytes, and
        // `chunk.len() <= GETENTROPY_MAX`, the documented per-call limit.
        let rc = unsafe { getentropy(chunk.as_mut_ptr().cast::<c_void>(), chunk.len()) };
        tracing::trace!(target: "ternvale::hv", len = chunk.len(), rc, "getentropy");
        if rc != 0 {
            let errno = std::io::Error::last_os_error().raw_os_error().unwrap_or(0);
            tracing::error!(target: "ternvale::hv", len = chunk.len(), errno, "getentropy failed");
            return Err(HvError::Entropy { errno });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{fill_entropy, GETENTROPY_MAX};

    #[test]
    fn fills_buffers_larger_than_one_call() {
        let mut a = vec![0u8; GETENTROPY_MAX * 3 + 17];
        let mut b = vec![0u8; a.len()];
        fill_entropy(&mut a).expect("entropy a");
        fill_entropy(&mut b).expect("entropy b");
        assert_ne!(a, b, "two draws differ");
        let tail = &a[GETENTROPY_MAX * 3..];
        assert!(tail.iter().any(|&x| x != 0), "last partial chunk filled");
        let zeros = a.iter().filter(|&&x| x == 0).count();
        assert!(
            zeros < a.len() / 16,
            "about 1/256 of bytes are zero, got {zeros}"
        );
    }

    #[test]
    fn empty_buffer_is_ok() {
        fill_entropy(&mut []).expect("empty");
    }
}
