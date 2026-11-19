//! Host-side Venus renderer for virtio-gpu 3D (`ternvale::gpu3d`).
//!
//! Chain: guest Mesa Venus → virtio-gpu CS → this crate → virglrenderer (optional)
//! → MoltenVK → Metal. Guest bytes are untrusted. Per-VM `gpu3d = false` by default
//! (see `ternvale-config` `[gpu]`).

mod align;
mod decoder;
mod error;
mod host;
mod mapping;
mod renderer;
mod state;
#[cfg(feature = "virgl")]
mod virgl;

pub use align::{HOSTMEM_BYTES, HOSTMEM_GPA, HV_PAGE, MOLTENVK_HOST_ALIGN, VIRTIO_BLOB_ALIGN};
pub use decoder::{check_object_ids, decode_submit, CsCommand, MAX_COMMANDS, MAX_SUBMIT};
pub use error::Gpu3dError;
pub use host::HostGpu;
pub use mapping::{GuestMapper, HvMapper, RecordingMapper};
pub use renderer::{HostRenderer, StubRenderer};
pub use state::{
    Blob, Fences, Gpu3dState, BLOB_MEM_GUEST, BLOB_MEM_HOST3D, BLOB_MEM_HOST3D_GUEST, CAPSET_VENUS,
};

#[cfg(feature = "virgl")]
pub use virgl::fill_venus_capset;

/// Decode-only entry used by cargo-fuzz (`gpu3d_decoder`). Never panics.
#[tracing::instrument(level = "trace", target = "ternvale::gpu3d", skip_all, fields(len = data.len()))]
pub fn fuzz_decode(data: &[u8]) {
    let _ = decode_submit(data);
}

#[cfg(test)]
mod live_tests;
#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-gpu3d-host");
    }
}
