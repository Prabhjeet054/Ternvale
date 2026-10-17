//! virtio-gpu for Ternvale guests, 2D-only profile (virtio 1.2 §5.7).
//!
//! [`VirtioGpu`] is a [`ternvale_devices::VirtioDevice`]: the same
//! virtio-mmio and virtio-pci transports that carry virtio-blk carry it
//! ([`VirtioGpu::attach`], [`VirtioGpu::attach_pci`]). A worker thread answers
//! the control queue: `GET_DISPLAY_INFO`, `RESOURCE_CREATE_2D`,
//! `RESOURCE_UNREF`, `RESOURCE_ATTACH_BACKING`, `RESOURCE_DETACH_BACKING`,
//! `SET_SCANOUT`, `TRANSFER_TO_HOST_2D` and `RESOURCE_FLUSH`. Each scanout
//! keeps a host-side pixel buffer ([`Frame`]) that [`GpuDisplay`] reads. The
//! cursor queue exists but its commands are not acted on yet; 3D, capsets,
//! EDID and blob resources answer `RESP_ERR_UNSPEC`.
//!
//! Logs go to `ternvale::gpu`: every command at DEBUG, every flush's timing
//! and every transfer at TRACE, scanout enable/disable at INFO, bad guest
//! input at WARN.
//!
//! [`Presenter`] (Step 44) streams one scanout to the dev viewer
//! (`tools/ternvale-viewer`) over a Unix socket in the [`present_protocol`]
//! format, capped at a frame rate; frame drops and flush-to-present latency
//! are logged at DEBUG.

mod device;
mod gpu2d;
mod present;
pub mod present_protocol;
mod present_session;
mod present_window;
pub mod protocol;
mod resource;
mod scanout;
#[cfg(test)]
mod test_log;
#[cfg(test)]
mod testutil;
mod worker;

pub use device::{
    AttachedGpu, GpuConfig, GpuError, PciGpu, VirtioGpu, CONFIG_LEN, DEFAULT_MODE, VIRTIO_GPU_ID,
};
pub use gpu2d::{GpuStats, MAX_DIMENSION, MAX_HOSTMEM};
pub use present::{
    PresentConfig, PresentError, PresentStats, Presenter, DEFAULT_FPS, DEFAULT_REPORT_EVERY,
    MAX_FPS,
};
pub use resource::{PixelFormat, BYTES_PER_PIXEL};
pub use scanout::{Frame, GpuDisplay};
pub use worker::{CONTROL_QUEUE, CURSOR_QUEUE, MAX_REQUEST};

#[cfg(test)]
mod tests {
    #[test]
    fn crate_name_is_stable() {
        assert_eq!(env!("CARGO_PKG_NAME"), "ternvale-gpu");
    }
}
