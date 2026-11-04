//! Safe wrappers around vmnet.framework shared (NAT) mode.
//!
//! Raw `vmnet_*` calls live in `vmnet_shim.c`. The unprivileged VMM talks to a
//! helper; only that helper should construct [`SharedInterface`].

use std::ffi::c_void;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const BATCH: usize = 32;
const STRIDE: usize = 65536;

#[link(name = "tv_vmnet_shim", kind = "static")]
unsafe extern "C" {
    fn tv_vmnet_start_shared(err: *mut i8, errlen: u32) -> *mut c_void;
    fn tv_vmnet_set_event_cb(
        s: *mut c_void,
        cb: Option<unsafe extern "C" fn(*mut c_void)>,
        ctx: *mut c_void,
    ) -> i32;
    fn tv_vmnet_read(
        s: *mut c_void,
        buf: *mut u8,
        stride: u32,
        n: u32,
        out_n: *mut u32,
        out_lens: *mut u32,
    ) -> i32;
    fn tv_vmnet_write(
        s: *mut c_void,
        buf: *const u8,
        lens: *const u32,
        stride: u32,
        n: u32,
        written: *mut u32,
    ) -> i32;
    fn tv_vmnet_stop(s: *mut c_void) -> i32;
    fn tv_vmnet_copy_info(
        s: *const c_void,
        mtu: *mut u64,
        max_packet: *mut u64,
        max_read: *mut u64,
        max_write: *mut u64,
        mac: *mut u8,
    );
    fn tv_vmnet_status_str(st: i32) -> *const i8;
}

/// Shared-mode NAT interface owned by the privileged helper.
pub struct SharedInterface {
    raw: *mut c_void,
    info: InterfaceInfo,
    ready: Arc<AtomicBool>,
}

// SAFETY: vmnet interface_ref is documented as usable from any thread; we
// serialize read/write on the helper's session thread.
unsafe impl Send for SharedInterface {}

/// MTU, max packet, batch sizes, and allocated MAC from vmnet.
#[derive(Debug, Clone, Copy)]
pub struct InterfaceInfo {
    pub mtu: u32,
    pub max_packet: u32,
    pub max_read: u32,
    pub max_write: u32,
    pub mac: [u8; 6],
}

impl SharedInterface {
    /// `vmnet_start_interface` with `VMNET_SHARED_MODE`. Needs root or
    /// `com.apple.vm.networking`.
    #[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
    pub fn start_shared() -> io::Result<Self> {
        let mut err = [0i8; 256];
        // SAFETY: err is a writable C string buffer.
        let raw = unsafe { tv_vmnet_start_shared(err.as_mut_ptr(), 256) };
        if raw.is_null() {
            let msg = c_err(&err);
            tracing::error!(
                target: "ternvale::net::vmnet",
                error = %msg,
                "vmnet_start_interface failed"
            );
            return Err(io::Error::other(msg));
        }
        let mut mtu = 0u64;
        let mut max_packet = 0u64;
        let mut max_read = 0u64;
        let mut max_write = 0u64;
        let mut mac = [0u8; 6];
        // SAFETY: `raw` is a live heap `tv_vmnet` from `tv_vmnet_start_shared`.
        unsafe {
            tv_vmnet_copy_info(
                raw,
                &mut mtu,
                &mut max_packet,
                &mut max_read,
                &mut max_write,
                mac.as_mut_ptr(),
            );
        }
        let info = InterfaceInfo {
            mtu: mtu as u32,
            max_packet: max_packet as u32,
            max_read: (max_read as u32).clamp(1, BATCH as u32),
            max_write: (max_write as u32).clamp(1, BATCH as u32),
            mac,
        };
        tracing::info!(
            target: "ternvale::net::vmnet",
            mtu = info.mtu,
            max_packet = info.max_packet,
            max_read = info.max_read,
            max_write = info.max_write,
            mac = %mac_str(&info.mac),
            "vmnet shared interface up"
        );
        let ready = Arc::new(AtomicBool::new(true));
        let iface = Self { raw, info, ready };
        iface.arm_events()?;
        Ok(iface)
    }

    fn arm_events(&self) -> io::Result<()> {
        let boxed = Box::new(Arc::clone(&self.ready));
        let ctx = Box::into_raw(boxed).cast();
        // SAFETY: `raw` is a live tv_vmnet; callback only stores on the Arc.
        let rc = unsafe { tv_vmnet_set_event_cb(self.raw, Some(on_packets), ctx) };
        if rc != 0 {
            unsafe {
                drop(Box::from_raw(ctx.cast::<Arc<AtomicBool>>()));
            }
            return Err(status_err("vmnet_interface_set_event_callback", rc));
        }
        Ok(())
    }

    #[tracing::instrument(level = "debug", target = "ternvale::net::vmnet", skip_all)]
    pub fn info(&self) -> InterfaceInfo {
        self.info
    }

    /// True after a PACKETS_AVAILABLE event (or initially so the first poll runs).
    #[tracing::instrument(level = "trace", target = "ternvale::net::vmnet", skip_all)]
    pub fn packets_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }

    /// `vmnet_read` up to `max_read` packets.
    #[tracing::instrument(level = "trace", target = "ternvale::net::vmnet", skip_all)]
    pub fn read_batch(&mut self) -> io::Result<Vec<Vec<u8>>> {
        self.ready.store(false, Ordering::Release);
        let n = self.info.max_read;
        let mut buf = vec![0u8; STRIDE * n as usize];
        let mut lens = vec![0u32; n as usize];
        let mut out_n = 0u32;
        // SAFETY: `self.raw` is live; `buf`/`lens` match `n` and STRIDE.
        let rc = unsafe {
            tv_vmnet_read(
                self.raw,
                buf.as_mut_ptr(),
                STRIDE as u32,
                n,
                &mut out_n,
                lens.as_mut_ptr(),
            )
        };
        tracing::trace!(target: "ternvale::net::vmnet", rc, count = out_n, "vmnet_read");
        if rc != 0 {
            return Err(status_err("vmnet_read", rc));
        }
        let mut frames = Vec::with_capacity(out_n as usize);
        for (i, pkt_len) in lens.iter().copied().enumerate().take(out_n as usize) {
            let len = pkt_len as usize;
            let start = i * STRIDE;
            if len == 0 || start + len > buf.len() {
                tracing::warn!(
                    target: "ternvale::net::vmnet",
                    len,
                    reason = "vmnet_read packet length invalid",
                    "packet drop"
                );
                continue;
            }
            frames.push(buf[start..start + len].to_vec());
        }
        Ok(frames)
    }

    /// `vmnet_write` a batch. Returns how many packets the kernel took.
    #[tracing::instrument(level = "trace", target = "ternvale::net::vmnet", skip_all)]
    pub fn write_batch(&mut self, frames: &[&[u8]]) -> io::Result<u32> {
        if frames.is_empty() {
            return Ok(0);
        }
        let cap = frames.len().min(self.info.max_write as usize).min(BATCH);
        let mut packed: Vec<&[u8]> = Vec::with_capacity(cap);
        for frame in frames.iter().take(cap) {
            if frame.is_empty() || frame.len() > STRIDE {
                tracing::warn!(
                    target: "ternvale::net::vmnet",
                    len = frame.len(),
                    reason = "frame exceeds vmnet buffer stride",
                    "packet drop"
                );
                continue;
            }
            packed.push(frame);
        }
        if packed.is_empty() {
            return Ok(0);
        }
        let n = packed.len();
        let mut buf = vec![0u8; STRIDE * n];
        let mut lens = vec![0u32; n];
        for (i, frame) in packed.iter().enumerate() {
            buf[i * STRIDE..i * STRIDE + frame.len()].copy_from_slice(frame);
            lens[i] = frame.len() as u32;
        }
        let mut written = 0u32;
        // SAFETY: `self.raw` is live; `buf`/`lens` describe `n` packets of STRIDE.
        let rc = unsafe {
            tv_vmnet_write(
                self.raw,
                buf.as_ptr(),
                lens.as_ptr(),
                STRIDE as u32,
                n as u32,
                &mut written,
            )
        };
        tracing::trace!(
            target: "ternvale::net::vmnet",
            rc,
            written,
            requested = n,
            "vmnet_write"
        );
        if rc != 0 {
            return Err(status_err("vmnet_write", rc));
        }
        Ok(written)
    }
}

impl Drop for SharedInterface {
    fn drop(&mut self) {
        tracing::info!(target: "ternvale::net::vmnet", "vmnet shared interface down");
        if self.raw.is_null() {
            return;
        }
        // SAFETY: `raw` is the heap object from start; stop frees it.
        let rc = unsafe { tv_vmnet_stop(self.raw) };
        self.raw = std::ptr::null_mut();
        if rc != 0 {
            tracing::warn!(
                target: "ternvale::net::vmnet",
                rc,
                status = status_name(rc),
                "vmnet_stop_interface failed"
            );
        }
    }
}

unsafe extern "C" fn on_packets(ctx: *mut c_void) {
    if ctx.is_null() {
        return;
    }
    // SAFETY: `ctx` is the leaked Box<Arc<AtomicBool>> from `arm_events`.
    let flag = unsafe { &*ctx.cast::<Arc<AtomicBool>>() };
    flag.store(true, Ordering::Release);
}

fn c_err(buf: &[i8]) -> String {
    let bytes: Vec<u8> = buf
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| *c as u8)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

fn status_name(rc: i32) -> &'static str {
    // SAFETY: the shim returns a static C string.
    let p = unsafe { tv_vmnet_status_str(rc) };
    if p.is_null() {
        return "UNKNOWN";
    }
    unsafe { std::ffi::CStr::from_ptr(p) }
        .to_str()
        .unwrap_or("UNKNOWN")
}

fn status_err(op: &str, rc: i32) -> io::Error {
    let name = status_name(rc);
    tracing::warn!(target: "ternvale::net::vmnet", op, rc, status = name, "vmnet call failed");
    io::Error::other(format!("{op}: {name} ({rc})"))
}

pub(crate) fn mac_str(mac: &[u8; 6]) -> String {
    format!(
        "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
        mac[0], mac[1], mac[2], mac[3], mac[4], mac[5]
    )
}

/// `VMNET_SHARED_MODE` from `<vmnet/vmnet.h>`.
pub const VMNET_SHARED_MODE: u32 = 1001;
