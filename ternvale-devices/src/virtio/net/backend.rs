//! Host side of a virtio-net device.

use super::loopback::LoopbackBackend;
use super::NetError;

/// Moves Ethernet frames (no virtio header, no FCS) between the device and the host.
///
/// The worker calls both methods from one thread. `recv` must not block: it
/// returns `Ok(None)` when no frame is waiting, and the worker polls again.
pub trait NetBackend: Send {
    /// Short name for logs, such as `loopback`.
    fn name(&self) -> &'static str;

    /// Take one guest TX frame.
    fn send(&mut self, frame: &[u8]) -> Result<(), NetError>;

    /// Next frame for the guest, if any.
    fn recv(&mut self) -> Result<Option<Vec<u8>>, NetError>;
}

/// Build the backend named in a config `nics[].backend` entry.
///
/// `loopback` always works. `vmnet` needs the `vmnet` cargo feature.
#[tracing::instrument(level = "debug", target = "ternvale::net", fields(name))]
pub fn open_backend(name: &str) -> Result<Box<dyn NetBackend>, NetError> {
    match name {
        "loopback" => {
            tracing::info!(target: "ternvale::net", backend = name, "net backend selected");
            Ok(Box::new(LoopbackBackend::new()))
        }
        "vmnet" => open_vmnet(),
        other => {
            tracing::error!(target: "ternvale::net", backend = other, "unknown net backend");
            Err(NetError::UnknownBackend {
                name: other.to_string(),
            })
        }
    }
}

#[cfg(feature = "vmnet")]
fn open_vmnet() -> Result<Box<dyn NetBackend>, NetError> {
    let backend = super::vmnet::VmnetBackend::open_shared()?;
    Ok(Box::new(backend))
}

#[cfg(not(feature = "vmnet"))]
fn open_vmnet() -> Result<Box<dyn NetBackend>, NetError> {
    tracing::error!(
        target: "ternvale::net",
        "vmnet backend requested but ternvale-devices was built without the vmnet feature"
    );
    Err(NetError::Unsupported {
        backend: "vmnet",
        reason: "built without the `vmnet` cargo feature",
    })
}
