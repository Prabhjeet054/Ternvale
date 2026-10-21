//! Runtime `libvirglrenderer` (MIT) for Venus when installed on the host (e.g. Homebrew).

use std::ffi::{c_char, c_int, c_void, CString};
use std::path::PathBuf;
use std::sync::Mutex;

use libloading::Library;

use super::capset::CAPSET_VENUS;

const VIRGL_RENDERER_VENUS: i32 = 1 << 6;
const VIRGL_RENDERER_USE_EGL: i32 = 1;
const VIRGL_RENDERER_THREAD_SYNC: i32 = 2;
const VIRGL_RENDERER_RENDER_SERVER: i32 = 1 << 9;

type InitFn = unsafe extern "C" fn(*mut c_void, c_int, *mut VirglCallbacks) -> c_int;
type SubmitFn = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
type ContextCreateFn = unsafe extern "C" fn(u32, u32, *const c_char) -> c_int;
type ContextDestroyFn = unsafe extern "C" fn(u32);
type GetCapSetFn = unsafe extern "C" fn(u32, *mut u32, *mut u32);
type FillCapsFn = unsafe extern "C" fn(u32, u32, *mut c_void);
type ResourceCreateBlobFn = unsafe extern "C" fn(*const ResourceCreateBlobArgs) -> c_int;
type CtxAttachFn = unsafe extern "C" fn(c_int, c_int);

#[repr(C)]
struct VirglCallbacks {
    version: c_int,
}

#[repr(C)]
struct Iovec {
    iov_base: *mut c_void,
    iov_len: usize,
}

#[repr(C)]
struct ResourceCreateBlobArgs {
    res_handle: u32,
    ctx_id: u32,
    blob_mem: u32,
    blob_flags: u32,
    blob_id: u64,
    size: u64,
    iovecs: *const Iovec,
    num_iovs: u32,
}

struct VirglLib {
    _lib: Library,
    init: InitFn,
    submit: SubmitFn,
    context_create: ContextCreateFn,
    context_destroy: ContextDestroyFn,
    get_cap_set: GetCapSetFn,
    fill_caps: FillCapsFn,
    resource_create_blob: ResourceCreateBlobFn,
    ctx_attach: CtxAttachFn,
}

static VIRGL: Mutex<Option<VirglLib>> = Mutex::new(None);

/// Try to load and initialize virglrenderer once.
#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all)]
pub fn ensure_loaded() -> bool {
    let mut slot = VIRGL.lock().expect("virgl mutex");
    if slot.is_some() {
        return true;
    }
    let path = virgl_library_path();
    let lib = match unsafe { Library::new(&path) } {
        Ok(lib) => lib,
        Err(error) => {
            tracing::info!(
                target: "ternvale::gpu",
                path = %path.display(),
                error = %error,
                "virglrenderer not loaded; Venus SUBMIT_3D stays stubbed"
            );
            return false;
        }
    };
    let loaded = match load_symbols(lib) {
        Ok(v) => v,
        Err(error) => {
            tracing::warn!(target: "ternvale::gpu", error = %error, "virglrenderer missing exports");
            return false;
        }
    };
    let flags = VIRGL_RENDERER_VENUS
        | VIRGL_RENDERER_USE_EGL
        | VIRGL_RENDERER_THREAD_SYNC
        | VIRGL_RENDERER_RENDER_SERVER;
    let callbacks = VirglCallbacks { version: 4 };
    let rc = unsafe {
        (loaded.init)(
            std::ptr::null_mut(),
            flags,
            &callbacks as *const _ as *mut _,
        )
    };
    if rc != 0 {
        tracing::error!(
            target: "ternvale::gpu",
            rc,
            "virgl_renderer_init failed (Venus needs EGL/Vulkan on this host)"
        );
        return false;
    }
    tracing::info!(target: "ternvale::gpu", path = %path.display(), "virglrenderer Venus backend ready");
    *slot = Some(loaded);
    true
}

fn load_symbols(lib: Library) -> Result<VirglLib, String> {
    unsafe {
        let init = *lib
            .get::<InitFn>(b"virgl_renderer_init\0")
            .map_err(|e| e.to_string())?;
        let submit = *lib
            .get::<SubmitFn>(b"virgl_renderer_submit_cmd\0")
            .map_err(|e| e.to_string())?;
        let context_create = *lib
            .get::<ContextCreateFn>(b"virgl_renderer_context_create\0")
            .map_err(|e| e.to_string())?;
        let context_destroy = *lib
            .get::<ContextDestroyFn>(b"virgl_renderer_context_destroy\0")
            .map_err(|e| e.to_string())?;
        let get_cap_set = *lib
            .get::<GetCapSetFn>(b"virgl_renderer_get_cap_set\0")
            .map_err(|e| e.to_string())?;
        let fill_caps = *lib
            .get::<FillCapsFn>(b"virgl_renderer_fill_caps\0")
            .map_err(|e| e.to_string())?;
        let resource_create_blob = *lib
            .get::<ResourceCreateBlobFn>(b"virgl_renderer_resource_create_blob\0")
            .map_err(|e| e.to_string())?;
        let ctx_attach = *lib
            .get::<CtxAttachFn>(b"virgl_renderer_ctx_attach_resource\0")
            .map_err(|e| e.to_string())?;
        Ok(VirglLib {
            _lib: lib,
            init,
            submit,
            context_create,
            context_destroy,
            get_cap_set,
            fill_caps,
            resource_create_blob,
            ctx_attach,
        })
    }
}

fn virgl_library_path() -> PathBuf {
    if let Ok(path) = std::env::var("TERNVALE_VIRGL_RENDERER") {
        return PathBuf::from(path);
    }
    for candidate in [
        "/opt/homebrew/lib/libvirglrenderer.1.dylib",
        "/opt/homebrew/opt/virglrenderer/lib/libvirglrenderer.1.dylib",
        "/usr/local/lib/libvirglrenderer.1.dylib",
        "libvirglrenderer.so.1",
    ] {
        if std::path::Path::new(candidate).exists() {
            return PathBuf::from(candidate);
        }
    }
    PathBuf::from("libvirglrenderer.1.dylib")
}

fn with_virgl<R>(f: impl FnOnce(&VirglLib) -> R) -> Option<R> {
    if !ensure_loaded() {
        return None;
    }
    let slot = VIRGL.lock().expect("virgl mutex");
    slot.as_ref().map(f)
}

// SAFETY: virglrenderer is documented thread-safe when THREAD_SYNC is set.
unsafe impl Send for VirglLib {}
unsafe impl Sync for VirglLib {}

/// Fill Venus capset bytes from virglrenderer when available.
#[tracing::instrument(level = "trace", target = "ternvale::gpu", skip(out))]
pub fn fill_venus_capset(out: &mut [u8]) {
    let filled = with_virgl(|lib| {
        let mut max_ver = 0u32;
        let mut max_size = 0u32;
        unsafe {
            (lib.get_cap_set)(CAPSET_VENUS, &mut max_ver, &mut max_size);
            if max_size as usize <= out.len() && max_ver > 0 {
                (lib.fill_caps)(CAPSET_VENUS, max_ver, out.as_mut_ptr() as *mut c_void);
                tracing::trace!(
                    target: "ternvale::gpu",
                    max_ver,
                    max_size,
                    "filled venus capset from virglrenderer"
                );
                return true;
            }
        }
        false
    });
    if filled != Some(true) {
        super::capset::fill_venus_capset_stub(out);
    }
}

#[allow(dead_code)]
impl VirglLib {
    fn create_blob(&self, args: &ResourceCreateBlobArgs) -> c_int {
        unsafe { (self.resource_create_blob)(args) }
    }

    fn attach_resource(&self, ctx_id: u32, res: u32) {
        unsafe { (self.ctx_attach)(ctx_id as c_int, res as c_int) };
    }
}

/// Create a virgl context; returns false on failure.
#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(ctx_id))]
pub fn context_create(ctx_id: u32, name: &str) -> bool {
    with_virgl(|lib| {
        let cname = CString::new(name).unwrap_or_else(|_| CString::new("ternvale").unwrap());
        let rc = unsafe {
            (lib.context_create)(ctx_id, cname.as_bytes().len() as u32, cname.as_ptr())
        };
        if rc != 0 {
            tracing::warn!(target: "ternvale::gpu", ctx_id, rc, "virgl_renderer_context_create failed");
            false
        } else {
            tracing::debug!(target: "ternvale::gpu", ctx_id, "virgl context created");
            true
        }
    })
    .unwrap_or(false)
}

#[tracing::instrument(level = "debug", target = "ternvale::gpu", skip_all, fields(ctx_id))]
pub fn context_destroy(ctx_id: u32) {
    let _ = with_virgl(|lib| unsafe { (lib.context_destroy)(ctx_id) });
}

/// Forward `SUBMIT_3D` payload to virglrenderer.
#[tracing::instrument(level = "trace", target = "ternvale::gpu", skip(payload))]
pub fn submit_3d(ctx_id: u32, payload: &[u8]) -> Result<(), i32> {
    if payload.len() % 4 != 0 {
        return Err(-2);
    }
    let ndw = (payload.len() / 4) as c_int;
    with_virgl(|lib| {
        let rc = unsafe { (lib.submit)(payload.as_ptr() as *mut c_void, ctx_id as c_int, ndw) };
        if rc != 0 {
            tracing::warn!(target: "ternvale::gpu", ctx_id, rc, ndw, "virgl_renderer_submit_cmd failed");
            Err(rc)
        } else {
            tracing::trace!(target: "ternvale::gpu", ctx_id, ndw, "virgl_renderer_submit_cmd ok");
            Ok(())
        }
    })
    .unwrap_or(Err(-1))
}
