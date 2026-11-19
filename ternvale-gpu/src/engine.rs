//! Control-queue engine: 2D scanout path plus optional Venus 3D (`gpu3d`).

use ternvale_vmm::GuestMemory;

use crate::gpu2d::Gpu2d;
#[cfg(feature = "gpu3d")]
use crate::gpu3d::Gpu3d;
#[cfg(feature = "gpu3d")]
use crate::protocol::{self, Command};
use crate::scanout::GpuDisplay;
use std::sync::Arc;

use crate::gpu2d::GpuStats;

/// 2D virtio-gpu state and, when compiled with `gpu3d` and enabled, the Venus path.
pub struct GpuEngine {
    #[cfg(feature = "gpu3d")]
    stats: Arc<GpuStats>,
    /// Scanout / 2D resources.
    pub two_d: Gpu2d,
    #[cfg(feature = "gpu3d")]
    three_d: Option<Gpu3d>,
}

impl GpuEngine {
    /// New device state for `display`. `gpu3d` is the per-VM flag (default false).
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(gpu3d))]
    pub fn new(display: GpuDisplay, stats: Arc<GpuStats>, gpu3d: bool) -> Self {
        #[cfg(feature = "gpu3d")]
        let three_d = gpu3d.then(|| Gpu3d::new(Arc::clone(&stats)));
        #[cfg(not(feature = "gpu3d"))]
        let _ = gpu3d;
        Self {
            #[cfg(feature = "gpu3d")]
            stats: Arc::clone(&stats),
            two_d: Gpu2d::new(display, Arc::clone(&stats)),
            #[cfg(feature = "gpu3d")]
            three_d,
        }
    }

    /// Reset 2D and 3D state.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn reset(&mut self) {
        self.two_d.reset();
        #[cfg(feature = "gpu3d")]
        if let Some(three_d) = self.three_d.as_mut() {
            three_d.reset();
        }
    }

    /// Answer one control-queue request.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
    pub fn handle(&mut self, mem: &GuestMemory, request: &[u8]) -> Vec<u8> {
        #[cfg(feature = "gpu3d")]
        {
            if self.three_d.is_some() {
                if let Ok((hdr, command)) = protocol::parse(request) {
                    if is_3d_command(&command) {
                        return self.handle_3d(mem, &hdr, &command);
                    }
                }
            }
        }
        self.two_d.handle(mem, request)
    }

    #[cfg(feature = "gpu3d")]
    fn handle_3d(
        &mut self,
        mem: &GuestMemory,
        hdr: &protocol::CtrlHdr,
        command: &Command,
    ) -> Vec<u8> {
        self.stats
            .commands
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let name = protocol::type_name(hdr.kind);
        tracing::debug!(
            target: "ternvale::gpu",
            cmd = name,
            kind = format!("{:#06x}", hdr.kind),
            fenced = hdr.flags & protocol::FLAG_FENCE != 0,
            fence_id = hdr.fence_id,
            ?command,
            "virtio-gpu 3d command"
        );
        let Some(three_d) = self.three_d.as_mut() else {
            return protocol::response_hdr(hdr, protocol::RESP_ERR_UNSPEC).to_vec();
        };
        match three_d.handle(mem, hdr, command) {
            Ok(bytes) if !bytes.is_empty() => bytes,
            Ok(_) => protocol::response_hdr(hdr, protocol::RESP_OK_NODATA).to_vec(),
            Err((kind, reason)) => {
                self.stats
                    .errors
                    .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                tracing::warn!(
                    target: "ternvale::gpu",
                    cmd = name,
                    response = protocol::type_name(kind),
                    reason = %reason,
                    "virtio-gpu 3d command failed"
                );
                protocol::response_hdr(hdr, kind).to_vec()
            }
        }
    }
}

#[cfg(feature = "gpu3d")]
fn is_3d_command(command: &Command) -> bool {
    matches!(
        command,
        Command::GetCapsetInfo { .. }
            | Command::GetCapset { .. }
            | Command::ResourceCreateBlob { .. }
            | Command::CtxCreate { .. }
            | Command::CtxDestroy
            | Command::CtxResource { .. }
            | Command::Submit3d { .. }
            | Command::BlobMap { .. }
    )
}
