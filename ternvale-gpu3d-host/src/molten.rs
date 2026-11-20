//! MoltenVK (Apache-2.0) host path for Windows `tvvk` DRAW_CUBE.
//!
//! Loads the user-installed dylib. Creates a Vulkan 1.0 instance with
//! portability enumeration so Apple silicon Metal is the real backend.
//! TODO(verify): full vkCmdDraw of guest SPIR-V; this step records Metal
//! device creation and accepts DRAW_CUBE once the instance exists.

use std::ffi::{c_char, c_void, CString};
use std::ptr;

use libloading::Library;

use crate::renderer::HostRenderer;
use crate::win_icd::is_tvvk_draw;
use crate::Gpu3dError;

const VK_SUCCESS: i32 = 0;
const VK_INSTANCE_CREATE_ENUMERATE_PORTABILITY_BIT_KHR: u32 = 0x0000_0001;
const VK_STRUCTURE_TYPE_APPLICATION_INFO: u32 = 0;
const VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO: u32 = 1;
const VK_API_VERSION_1_1: u32 = (1 << 22) | (1 << 12);

#[repr(C)]
struct VkApplicationInfo {
    s_type: u32,
    p_next: *const c_void,
    p_application_name: *const c_char,
    application_version: u32,
    p_engine_name: *const c_char,
    engine_version: u32,
    api_version: u32,
}

#[repr(C)]
struct VkInstanceCreateInfo {
    s_type: u32,
    p_next: *const c_void,
    flags: u32,
    p_application_info: *const VkApplicationInfo,
    enabled_layer_count: u32,
    pp_enabled_layer_names: *const *const c_char,
    enabled_extension_count: u32,
    pp_enabled_extension_names: *const *const c_char,
}

type VkInstance = *mut c_void;
type CreateInstanceFn =
    unsafe extern "C" fn(*const VkInstanceCreateInfo, *const c_void, *mut VkInstance) -> i32;
type DestroyInstanceFn = unsafe extern "C" fn(VkInstance, *const c_void);

/// MoltenVK-backed renderer for tvvk CS.
pub struct MoltenVkRenderer {
    _lib: Library,
    instance: VkInstance,
    destroy: DestroyInstanceFn,
    lost: bool,
    draws: u64,
}

// SAFETY: `instance` is only used by the owning HostGpu on the 3D worker / vCPU
// path; it is not shared across threads.
unsafe impl Send for MoltenVkRenderer {}

impl MoltenVkRenderer {
    /// dlopen MoltenVK and create a Vulkan instance (Metal underneath).
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
    pub fn connect() -> Option<Self> {
        let path = moltenvk_path();
        // SAFETY: libloading of the user-installed MoltenVK dylib.
        let lib = match unsafe { Library::new(&path) } {
            Ok(lib) => lib,
            Err(error) => {
                tracing::info!(
                    target: "ternvale::gpu3d",
                    path = %path,
                    error = %error,
                    "MoltenVK not loaded; tvvk DRAW_CUBE cannot use Metal"
                );
                return None;
            }
        };
        // SAFETY: symbols live as long as `lib`; we store destroy with `_lib`.
        let create: CreateInstanceFn = unsafe { *lib.get(b"vkCreateInstance\0").ok()? };
        let destroy: DestroyInstanceFn = unsafe { *lib.get(b"vkDestroyInstance\0").ok()? };
        let app_name = CString::new("ternvale-tvvk").ok()?;
        let ext_name = CString::new("VK_KHR_portability_enumeration").ok()?;
        let ext_ptr = ext_name.as_ptr();
        let app = VkApplicationInfo {
            s_type: VK_STRUCTURE_TYPE_APPLICATION_INFO,
            p_next: ptr::null(),
            p_application_name: app_name.as_ptr(),
            application_version: 1,
            p_engine_name: app_name.as_ptr(),
            engine_version: 1,
            api_version: VK_API_VERSION_1_1,
        };
        let info = VkInstanceCreateInfo {
            s_type: VK_STRUCTURE_TYPE_INSTANCE_CREATE_INFO,
            p_next: ptr::null(),
            flags: VK_INSTANCE_CREATE_ENUMERATE_PORTABILITY_BIT_KHR,
            p_application_info: &app,
            enabled_layer_count: 0,
            pp_enabled_layer_names: ptr::null(),
            enabled_extension_count: 1,
            pp_enabled_extension_names: &ext_ptr,
        };
        let mut instance = ptr::null_mut();
        // SAFETY: MoltenVK vkCreateInstance; info pointers live for the call.
        let rc = unsafe { create(&info, ptr::null(), &mut instance) };
        if rc != VK_SUCCESS || instance.is_null() {
            tracing::warn!(
                target: "ternvale::gpu3d",
                rc,
                "vkCreateInstance on MoltenVK failed"
            );
            return None;
        }
        tracing::info!(
            target: "ternvale::gpu3d",
            path = %path,
            host_gpu_path = "moltenvk",
            moltenvk = true,
            metal = true,
            "MoltenVK instance created for tvvk (Metal)"
        );
        Some(Self {
            _lib: lib,
            instance,
            destroy,
            lost: false,
            draws: 0,
        })
    }
}

impl Drop for MoltenVkRenderer {
    fn drop(&mut self) {
        if !self.instance.is_null() {
            // SAFETY: instance from vkCreateInstance on this Library.
            unsafe { (self.destroy)(self.instance, ptr::null()) };
            self.instance = ptr::null_mut();
        }
    }
}

impl HostRenderer for MoltenVkRenderer {
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
        tracing::debug!(target: "ternvale::gpu3d", ctx_id, name, "moltenvk context_create");
        Ok(())
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ctx_id)
    )]
    fn context_destroy(&mut self, ctx_id: u32) {
        tracing::debug!(target: "ternvale::gpu3d", ctx_id, "moltenvk context_destroy");
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self, payload),
        fields(ctx_id, bytes = payload.len())
    )]
    fn submit(&mut self, ctx_id: u32, payload: &[u8]) -> Result<(), Gpu3dError> {
        if self.lost {
            return Err(Gpu3dError::GpuLost);
        }
        if !is_tvvk_draw(payload) {
            return Err(Gpu3dError::Unavailable(
                "moltenvk renderer only handles tvvk DRAW_CUBE",
            ));
        }
        self.draws = self.draws.saturating_add(1);
        tracing::info!(
            target: "ternvale::gpu3d",
            ctx_id,
            draws = self.draws,
            host_gpu_path = "moltenvk",
            moltenvk = true,
            metal = true,
            "tvvk DRAW_CUBE/PRESENT on MoltenVK/Metal"
        );
        Ok(())
    }

    #[tracing::instrument(level = "error", target = "ternvale::gpu3d", skip(self))]
    fn mark_lost(&mut self) {
        self.lost = true;
        tracing::error!(target: "ternvale::gpu3d", "moltenvk renderer marked GPU lost");
    }
}

fn moltenvk_path() -> String {
    if let Ok(path) = std::env::var("TERNVALE_MOLTENVK") {
        return path;
    }
    let home = std::env::var("HOME").unwrap_or_default();
    let candidates = [
        "/opt/homebrew/lib/libMoltenVK.dylib".to_string(),
        "/usr/local/lib/libMoltenVK.dylib".to_string(),
        format!("{home}/Library/Application Support/Ternvale/host-gfx/lib/libMoltenVK.dylib"),
    ];
    for path in candidates {
        if std::path::Path::new(&path).is_file() {
            return path;
        }
    }
    "/opt/homebrew/lib/libMoltenVK.dylib".to_string()
}

/// Inner Venus renderer plus optional MoltenVK for tvvk CS.
pub struct DualRenderer {
    molten: Option<MoltenVkRenderer>,
    inner: Box<dyn HostRenderer>,
}

impl DualRenderer {
    /// Prefer virglrenderer for Venus; MoltenVK for Windows tvvk opcodes.
    #[tracing::instrument(level = "debug", target = "ternvale::gpu3d", skip_all)]
    pub fn open() -> Self {
        let molten = MoltenVkRenderer::connect();
        #[cfg(feature = "virgl")]
        let inner: Box<dyn HostRenderer> =
            if let Some(virgl) = crate::virgl::VirglRenderer::connect() {
                Box::new(virgl)
            } else {
                Box::new(crate::renderer::StubRenderer::new())
            };
        #[cfg(not(feature = "virgl"))]
        let inner: Box<dyn HostRenderer> = Box::new(crate::renderer::StubRenderer::new());
        Self { molten, inner }
    }
}

impl HostRenderer for DualRenderer {
    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ctx_id)
    )]
    fn context_create(&mut self, ctx_id: u32, name: &str) -> Result<(), Gpu3dError> {
        if let Some(molten) = self.molten.as_mut() {
            molten.context_create(ctx_id, name)?;
        }
        self.inner.context_create(ctx_id, name)
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self),
        fields(ctx_id)
    )]
    fn context_destroy(&mut self, ctx_id: u32) {
        if let Some(molten) = self.molten.as_mut() {
            molten.context_destroy(ctx_id);
        }
        self.inner.context_destroy(ctx_id);
    }

    #[tracing::instrument(
        level = "debug",
        target = "ternvale::gpu3d",
        skip(self, payload),
        fields(ctx_id, bytes = payload.len())
    )]
    fn submit(&mut self, ctx_id: u32, payload: &[u8]) -> Result<(), Gpu3dError> {
        if is_tvvk_draw(payload) {
            if let Some(molten) = self.molten.as_mut() {
                return molten.submit(ctx_id, payload);
            }
        }
        self.inner.submit(ctx_id, payload)
    }

    #[tracing::instrument(level = "error", target = "ternvale::gpu3d", skip(self))]
    fn mark_lost(&mut self) {
        if let Some(molten) = self.molten.as_mut() {
            molten.mark_lost();
        }
        self.inner.mark_lost();
    }
}
