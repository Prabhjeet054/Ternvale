//! C ABI bindings. The only `unsafe` in this crate.

use std::ffi::{CStr, CString};
use std::os::raw::{c_char, c_void};
use std::sync::OnceLock;

use crate::error::VzError;
use crate::plan::VzPlan;

pub type Handle = i32;

const ERR_LEN: usize = 512;

static LOGGER: OnceLock<()> = OnceLock::new();

extern "C" {
    fn tv_vz_set_logger(fn_: Option<extern "C" fn(i32, *const c_char)>);
    fn tv_vz_create(
        name: *const c_char,
        cpus: u32,
        ram_bytes: u64,
        kernel: *const c_char,
        initrd: *const c_char,
        cmdline: *const c_char,
        nvram: *const c_char,
        serial_log: *const c_char,
        uds_dir: *const c_char,
        ports: *const u32,
        port_count: u32,
        disks: *const c_char,
        disk_ro: *const u8,
        disk_count: u32,
        attach_vsock: u8,
        attach_nat: u8,
        mac_hw_hex: *const c_char,
        mac_id_hex: *const c_char,
        mac_aux: *const c_char,
        devices: *const c_char,
        cb: Option<extern "C" fn(*mut c_void, i32, *const c_char)>,
        ctx: *mut c_void,
        err: *mut c_char,
        err_len: usize,
    ) -> i32;
    fn tv_vz_start(handle: i32, err: *mut c_char, err_len: usize) -> i32;
    fn tv_vz_pause(handle: i32, err: *mut c_char, err_len: usize) -> i32;
    fn tv_vz_resume(handle: i32, err: *mut c_char, err_len: usize) -> i32;
    fn tv_vz_stop(handle: i32, force: u8, err: *mut c_char, err_len: usize) -> i32;
    fn tv_vz_state(handle: i32) -> i32;
    fn tv_vz_destroy(handle: i32);
    fn tv_vz_nsapp_run();
    fn tv_vz_nsapp_stop();
    fn tv_vz_save(handle: i32, path: *const c_char, err: *mut c_char, err_len: usize) -> i32;
    fn tv_vz_restore(handle: i32, path: *const c_char, err: *mut c_char, err_len: usize) -> i32;
}

extern "C" fn rust_log(level: i32, message: *const c_char) {
    let text = unsafe { cstr(message) };
    match level {
        1 => tracing::error!(target: "ternvale::vz", "{text}"),
        2 => tracing::warn!(target: "ternvale::vz", "{text}"),
        3 => tracing::info!(target: "ternvale::vz", "{text}"),
        4 => tracing::debug!(target: "ternvale::vz", "{text}"),
        _ => tracing::trace!(target: "ternvale::vz", "{text}"),
    }
}

pub(crate) fn ensure_logger() {
    install_logger();
}

/// SAFETY: `ptr` is null or a NUL-terminated UTF-8 C string that lives for this call.
pub(crate) unsafe fn cstr_owned(ptr: *const c_char) -> String {
    // SAFETY: caller invariant above.
    cstr(ptr)
}

pub(crate) fn cstring(s: &str) -> Result<CString, VzError> {
    CString::new(s).map_err(|_| VzError::Host {
        op: "cstring",
        detail: "config string contains interior NUL".into(),
    })
}

pub(crate) fn read_err(buf: &[c_char], rc: i32, op: &'static str) -> Result<(), VzError> {
    if rc >= 0 {
        return Ok(());
    }
    let detail = unsafe { cstr(buf.as_ptr()) };
    tracing::error!(target: "ternvale::vz", op, detail = %detail, rc, "vz ffi failed");
    Err(VzError::Host {
        op,
        detail: if detail.is_empty() {
            format!("rc={rc}")
        } else {
            detail
        },
    })
}

unsafe fn cstr(ptr: *const c_char) -> String {
    if ptr.is_null() {
        return String::new();
    }
    // SAFETY: Swift passes a NUL-terminated UTF-8 C string that lives for
    // the duration of the callback.
    CStr::from_ptr(ptr).to_string_lossy().into_owned()
}

fn install_logger() {
    LOGGER.get_or_init(|| {
        // SAFETY: tv_vz_set_logger stores the function pointer; rust_log
        // only reads the C string for the call.
        unsafe { tv_vz_set_logger(Some(rust_log)) };
    });
}

/// Create a live VZ virtual machine. `cb` is invoked on the VM dispatch queue.
#[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(vm = %plan.name))]
pub fn create(
    plan: &VzPlan,
    cb: Option<extern "C" fn(*mut c_void, i32, *const c_char)>,
    ctx: *mut c_void,
) -> Result<Handle, VzError> {
    install_logger();
    let name = cstring(&plan.name)?;
    let kernel = cstring(&plan.kernel)?;
    let initrd = cstring(&plan.initrd)?;
    let cmdline = cstring(&plan.cmdline)?;
    let nvram = cstring(&plan.nvram)?;
    let serial = cstring(&plan.serial_log)?;
    let uds = cstring(&plan.uds_dir)?;
    let disk_blob = plan
        .disks
        .iter()
        .map(|(p, _)| p.as_str())
        .collect::<Vec<_>>()
        .join("\n");
    let disks = cstring(&disk_blob)?;
    let mac_hw = cstring(&plan.macos_hw_hex)?;
    let mac_id = cstring(&plan.macos_id_hex)?;
    let mac_aux = cstring(&plan.macos_aux)?;
    let devices = cstring(&plan.devices_blob())?;
    let ro: Vec<u8> = plan.disks.iter().map(|(_, ro)| u8::from(*ro)).collect();
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: pointers are live CStrings; `cb`/`ctx` are held by the Swift
    // session until destroy; err is a stack buffer of ERR_LEN bytes.
    let handle = unsafe {
        tv_vz_create(
            name.as_ptr(),
            plan.cpus,
            plan.ram_bytes,
            kernel.as_ptr(),
            initrd.as_ptr(),
            cmdline.as_ptr(),
            nvram.as_ptr(),
            serial.as_ptr(),
            uds.as_ptr(),
            plan.listen_ports.as_ptr(),
            plan.listen_ports.len() as u32,
            disks.as_ptr(),
            ro.as_ptr(),
            plan.disks.len() as u32,
            u8::from(plan.attach_vsock),
            u8::from(plan.attach_nat),
            mac_hw.as_ptr(),
            mac_id.as_ptr(),
            mac_aux.as_ptr(),
            devices.as_ptr(),
            cb,
            ctx,
            err.as_mut_ptr(),
            err.len(),
        )
    };
    if handle < 0 {
        read_err(&err, handle, "create")?;
        return Err(VzError::Host {
            op: "create",
            detail: "unknown".into(),
        });
    }
    tracing::debug!(target: "ternvale::vz", handle, "vz handle created");
    Ok(handle)
}

#[tracing::instrument(level = "trace", target = "ternvale::vz", skip_all, fields(handle))]
pub fn start(handle: Handle) -> Result<(), VzError> {
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: handle was returned by create and not yet destroyed.
    let rc = unsafe { tv_vz_start(handle, err.as_mut_ptr(), err.len()) };
    read_err(&err, rc, "start")
}

#[tracing::instrument(level = "trace", target = "ternvale::vz", skip_all, fields(handle))]
pub fn pause(handle: Handle) -> Result<(), VzError> {
    let mut err = [0 as c_char; ERR_LEN];
    let rc = unsafe { tv_vz_pause(handle, err.as_mut_ptr(), err.len()) };
    read_err(&err, rc, "pause")
}

#[tracing::instrument(level = "trace", target = "ternvale::vz", skip_all, fields(handle))]
pub fn resume(handle: Handle) -> Result<(), VzError> {
    let mut err = [0 as c_char; ERR_LEN];
    let rc = unsafe { tv_vz_resume(handle, err.as_mut_ptr(), err.len()) };
    read_err(&err, rc, "resume")
}

#[tracing::instrument(
    level = "trace",
    target = "ternvale::vz",
    skip_all,
    fields(handle, force)
)]
pub fn stop(handle: Handle, force: bool) -> Result<(), VzError> {
    let mut err = [0 as c_char; ERR_LEN];
    let rc = unsafe { tv_vz_stop(handle, u8::from(force), err.as_mut_ptr(), err.len()) };
    read_err(&err, rc, "stop")
}

#[tracing::instrument(level = "trace", target = "ternvale::vz", skip_all, fields(handle))]
pub fn state(handle: Handle) -> i32 {
    // SAFETY: handle is a live session or Swift returns failed.
    unsafe { tv_vz_state(handle) }
}

/// Block the calling thread on `NSApplication.run` (CLI main thread).
#[tracing::instrument(level = "info", target = "ternvale::vz", skip_all)]
pub fn nsapp_run() {
    tracing::info!(target: "ternvale::vz", "vz NSApplication.run (main thread)");
    // SAFETY: Swift `tv_vz_nsapp_run` runs AppKit; must be the process main thread.
    unsafe { tv_vz_nsapp_run() }
}

/// Unblock [`nsapp_run`] after the VM reaches a terminal state.
#[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all)]
pub fn nsapp_stop() {
    tracing::debug!(target: "ternvale::vz", "vz NSApplication.stop");
    // SAFETY: posts stop onto the main queue; safe from any thread.
    unsafe { tv_vz_nsapp_stop() }
}

#[tracing::instrument(
    level = "debug",
    target = "ternvale::vz",
    skip_all,
    fields(handle, path)
)]
pub fn save(handle: Handle, path: &str) -> Result<(), VzError> {
    let c_path = cstring(path)?;
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: handle is live; path/err live for the call.
    let rc = unsafe { tv_vz_save(handle, c_path.as_ptr(), err.as_mut_ptr(), err.len()) };
    read_err(&err, rc, "save")
}

#[tracing::instrument(
    level = "debug",
    target = "ternvale::vz",
    skip_all,
    fields(handle, path)
)]
pub fn restore(handle: Handle, path: &str) -> Result<(), VzError> {
    let c_path = cstring(path)?;
    let mut err = [0 as c_char; ERR_LEN];
    // SAFETY: handle is live; path/err live for the call.
    let rc = unsafe { tv_vz_restore(handle, c_path.as_ptr(), err.as_mut_ptr(), err.len()) };
    read_err(&err, rc, "restore")
}

#[tracing::instrument(level = "debug", target = "ternvale::vz", skip_all, fields(handle))]
pub fn destroy(handle: Handle) {
    // SAFETY: destroys the Swift session; handle must not be reused.
    unsafe { tv_vz_destroy(handle) };
}
