//! Runtime `libvirglrenderer` (MIT) Venus backend: guest CS → host Vulkan (MoltenVK).

use std::ffi::{c_char, c_int, c_void, CString};
use std::path::PathBuf;
use std::sync::Mutex;

use libloading::Library;

use crate::renderer::HostRenderer;
use crate::state::CAPSET_VENUS;
use crate::Gpu3dError;

const VIRGL_RENDERER_VENUS: i32 = 1 << 6;
const VIRGL_RENDERER_NO_VIRGL: i32 = 1 << 7;
const VIRGL_RENDERER_THREAD_SYNC: i32 = 2;
const VIRGL_RENDERER_USE_SURFACELESS: i32 = 1 << 3;

type InitFn = unsafe extern "C" fn(*mut c_void, c_int, *mut VirglCallbacks) -> c_int;
type SubmitFn = unsafe extern "C" fn(*mut c_void, c_int, c_int) -> c_int;
type ContextCreateFn = unsafe extern "C" fn(u32, u32, *const c_char) -> c_int;
type ContextDestroyFn = unsafe extern "C" fn(u32);
type GetCapSetFn = unsafe extern "C" fn(u32, *mut u32, *mut u32);
type FillCapsFn = unsafe extern "C" fn(u32, u32, *mut c_void);

#[repr(C)]
struct VirglCallbacks {
    version: c_int,
}

struct VirglLib {
    _lib: Library,
    init: InitFn,
    submit: SubmitFn,
    context_create: ContextCreateFn,
    context_destroy: ContextDestroyFn,
    get_cap_set: GetCapSetFn,
    fill_caps: FillCapsFn,
}

static VIRGL: Mutex<Option<VirglLib>> = Mutex::new(None);

fn lock_slot() -> std::sync::MutexGuard<'static, Option<VirglLib>> {
    VIRGL.lock().unwrap_or_else(|poison| poison.into_inner())
}

/// Try to load and initialize virglrenderer once.
#[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
pub fn ensure_loaded() -> bool {
    let mut slot = lock_slot();
    if slot.is_some() {
        return true;
    }
    prefer_moltenvk_icd();
    let path = virgl_library_path();
    // SAFETY: `libloading` dlopen of the virglrenderer dylib; failure is Err.
    let lib = match unsafe { Library::new(&path) } {
        Ok(lib) => lib,
        Err(error) => {
            tracing::info!(
                target: "ternvale::gpu3d",
                path = %path.display(),
                error = %error,
                "virglrenderer not loaded; Venus stays on the stub renderer"
            );
            return false;
        }
    };
    let loaded = match load_symbols(lib) {
        Ok(v) => v,
        Err(error) => {
            tracing::warn!(target: "ternvale::gpu3d", error = %error, "virglrenderer missing exports");
            return false;
        }
    };
    // macOS Seatbelt denies process-fork; skip RENDER_SERVER. EGL is Linux-only.
    // Callback version 1 matches virglrenderer's documented minimum (full v4
    // needs extra function pointers we do not implement).
    // TODO(verify): VIRGL_RENDERER_VENUS + MoltenVK on stock vs -Dvenus=true builds.
    let flag_sets = [
        VIRGL_RENDERER_VENUS
            | VIRGL_RENDERER_NO_VIRGL
            | VIRGL_RENDERER_THREAD_SYNC
            | VIRGL_RENDERER_USE_SURFACELESS,
        VIRGL_RENDERER_VENUS | VIRGL_RENDERER_NO_VIRGL | VIRGL_RENDERER_THREAD_SYNC,
        VIRGL_RENDERER_NO_VIRGL | VIRGL_RENDERER_THREAD_SYNC,
    ];
    let mut rc = -1;
    let mut flags_used = 0;
    for flags in flag_sets {
        let callbacks = VirglCallbacks { version: 1 };
        // SAFETY: init is the symbol from this Library; callbacks live for the call.
        rc = unsafe {
            (loaded.init)(
                std::ptr::null_mut(),
                flags,
                &callbacks as *const _ as *mut _,
            )
        };
        flags_used = flags;
        if rc == 0 {
            break;
        }
        tracing::debug!(
            target: "ternvale::gpu3d",
            rc,
            flags,
            "virgl_renderer_init rejected this flag set"
        );
    }
    if rc != 0 {
        tracing::error!(
            target: "ternvale::gpu3d",
            rc,
            flags = flags_used,
            "virgl_renderer_init failed (Venus needs MoltenVK/Vulkan on this host)"
        );
        return false;
    }
    let mut venus_ver = 0u32;
    let mut venus_size = 0u32;
    // SAFETY: get_cap_set is loaded; out-params are stack.
    unsafe {
        (loaded.get_cap_set)(CAPSET_VENUS, &mut venus_ver, &mut venus_size);
    }
    if venus_size == 0 {
        tracing::warn!(
            target: "ternvale::gpu3d",
            path = %path.display(),
            flags = flags_used,
            venus_ver,
            "libvirglrenderer loaded but Venus capset 4 is empty; rebuild with -Dvenus=true (scripts/install-host-virgl.sh)"
        );
    }
    tracing::info!(
        target: "ternvale::gpu3d",
        path = %path.display(),
        flags = flags_used,
        venus_ver,
        venus_size,
        host_gpu_path = "virgl_moltenvk",
        moltenvk = true,
        metal = true,
        "virglrenderer Venus backend ready (MoltenVK/Metal)"
    );
    *slot = Some(loaded);
    true
}

fn load_symbols(lib: Library) -> Result<VirglLib, String> {
    // SAFETY: each `get` looks up a documented virglrenderer export; Library owns the dylib.
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
        Ok(VirglLib {
            _lib: lib,
            init,
            submit,
            context_create,
            context_destroy,
            get_cap_set,
            fill_caps,
        })
    }
}

fn virgl_library_path() -> PathBuf {
    if let Ok(path) = std::env::var("TERNVALE_VIRGL_RENDERER") {
        return PathBuf::from(path);
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".into());
    let local = PathBuf::from(&home)
        .join("Library/Application Support/Ternvale/host-gfx/lib/libvirglrenderer.1.dylib");
    for candidate in [
        local.as_path(),
        std::path::Path::new("/opt/homebrew/lib/libvirglrenderer.1.dylib"),
        std::path::Path::new("/opt/homebrew/opt/virglrenderer/lib/libvirglrenderer.1.dylib"),
        std::path::Path::new("/usr/local/lib/libvirglrenderer.1.dylib"),
        std::path::Path::new("libvirglrenderer.so.1"),
    ] {
        if candidate.exists() {
            return candidate.to_path_buf();
        }
    }
    PathBuf::from("libvirglrenderer.1.dylib")
}

/// Prefer MoltenVK over Homebrew Mesa lavapipe when the operator did not set an ICD.
#[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
fn prefer_moltenvk_icd() {
    if std::env::var_os("VK_DRIVER_FILES").is_some()
        || std::env::var_os("VK_ICD_FILENAMES").is_some()
    {
        return;
    }
    for candidate in [
        "/opt/homebrew/etc/vulkan/icd.d/MoltenVK_icd.json",
        "/opt/homebrew/share/vulkan/icd.d/MoltenVK_icd.json",
        "/opt/homebrew/opt/molten-vk/etc/vulkan/icd.d/MoltenVK_icd.json",
        "/opt/homebrew/opt/molten-vk/share/vulkan/icd.d/MoltenVK_icd.json",
        "/usr/local/etc/vulkan/icd.d/MoltenVK_icd.json",
    ] {
        if !std::path::Path::new(candidate).exists() {
            continue;
        }
        // SAFETY: called once from ensure_loaded before worker threads submit CS.
        unsafe {
            std::env::set_var("VK_DRIVER_FILES", candidate);
            std::env::set_var("VK_ICD_FILENAMES", candidate);
        }
        tracing::info!(
            target: "ternvale::gpu3d",
            icd = candidate,
            "selected MoltenVK ICD for host Venus"
        );
        return;
    }
}

fn with_virgl<R>(f: impl FnOnce(&VirglLib) -> R) -> Option<R> {
    if !ensure_loaded() {
        return None;
    }
    let slot = lock_slot();
    slot.as_ref().map(f)
}

// SAFETY: virglrenderer is documented thread-safe when THREAD_SYNC is set.
unsafe impl Send for VirglLib {}
unsafe impl Sync for VirglLib {}

/// Fill Venus capset bytes from virglrenderer when available.
#[tracing::instrument(level = "trace", target = "ternvale::gpu3d", skip(out))]
pub fn fill_venus_capset(out: &mut [u8]) -> bool {
    with_virgl(|lib| {
        let mut max_ver = 0u32;
        let mut max_size = 0u32;
        // SAFETY: symbols came from load_symbols; out-params are stack / `out`.
        unsafe {
            (lib.get_cap_set)(CAPSET_VENUS, &mut max_ver, &mut max_size);
            // virglrenderer reports Venus max_ver as 0; size is the blob length.
            if max_size > 0 && (max_size as usize) <= out.len() {
                (lib.fill_caps)(CAPSET_VENUS, max_ver, out.as_mut_ptr() as *mut c_void);
                return true;
            }
        }
        false
    })
    .unwrap_or(false)
}

/// Renderer that forwards to the process-wide virglrenderer instance.
pub struct VirglRenderer {
    lost: bool,
}

impl VirglRenderer {
    /// Load virglrenderer; None if the dylib is missing or init failed.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
    pub fn connect() -> Option<Self> {
        if ensure_loaded() {
            Some(Self { lost: false })
        } else {
            None
        }
    }
}

impl HostRenderer for VirglRenderer {
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ctx_id)
    )]
    fn context_create(&mut self, ctx_id: u32, name: &str) -> Result<(), Gpu3dError> {
        if self.lost {
            return Err(Gpu3dError::GpuLost);
        }
        let cleaned: Vec<u8> = name.bytes().filter(|b| *b != 0).collect();
        let cname = CString::new(cleaned).unwrap_or_else(|_| CString::default());
        let rc = with_virgl(|lib| {
            // SAFETY: `cname` is a live CString; virglrenderer copies the name.
            unsafe { (lib.context_create)(ctx_id, cname.as_bytes().len() as u32, cname.as_ptr()) }
        })
        .ok_or(Gpu3dError::Unavailable("virglrenderer gone"))?;
        if rc != 0 {
            tracing::warn!(target: "ternvale::gpu3d", ctx_id, rc, "virgl_renderer_context_create failed");
            Err(Gpu3dError::Renderer(rc))
        } else {
            Ok(())
        }
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ctx_id)
    )]
    fn context_destroy(&mut self, ctx_id: u32) {
        let _ = with_virgl(|lib| {
            // SAFETY: ctx_id is a virgl context this process created, or virgl no-ops.
            unsafe { (lib.context_destroy)(ctx_id) }
        });
    }

    #[tracing::instrument(
        level = "trace",
        target = "ternvale::gpu3d",
        skip(self, payload),
        fields(ctx_id, bytes = payload.len())
    )]
    fn submit(&mut self, ctx_id: u32, payload: &[u8]) -> Result<(), Gpu3dError> {
        if self.lost {
            return Err(Gpu3dError::GpuLost);
        }
        if payload.len() % 4 != 0 {
            return Err(Gpu3dError::Decode("payload is not dword-aligned"));
        }
        let ndw = (payload.len() / 4) as c_int;
        let rc = with_virgl(|lib| {
            // SAFETY: payload length is a multiple of 4; virglrenderer reads `ndw` dwords.
            unsafe { (lib.submit)(payload.as_ptr() as *mut c_void, ctx_id as c_int, ndw) }
        })
        .ok_or(Gpu3dError::Unavailable("virglrenderer gone"))?;
        if rc != 0 {
            tracing::warn!(
                target: "ternvale::gpu3d",
                ctx_id,
                rc,
                ndw,
                "virgl_renderer_submit_cmd failed"
            );
            Err(Gpu3dError::Renderer(rc))
        } else {
            Ok(())
        }
    }

    #[tracing::instrument(level = "error", target = "ternvale::gpu3d", skip(self))]
    fn mark_lost(&mut self) {
        self.lost = true;
        tracing::error!(
            target: "ternvale::gpu3d",
            "virglrenderer GPU marked lost; VM continues"
        );
    }
}
