//! Host IPP proxy: vsock `host-8631.sock` → CUPS print-job ops or Preview PDF.

mod cups;
mod http;
mod ops;
mod pdf;
mod policy;
mod proxy;
#[cfg(test)]
mod quality;

pub use cups::{list_host_queues, parse_lpstat};
pub use http::inspect;
pub use pdf::open_preview;
pub use policy::{noop_open, PrintPolicy};
pub use proxy::{dispatch, PrintProxy, PrintProxyError};
