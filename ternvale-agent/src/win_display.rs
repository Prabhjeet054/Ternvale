//! Windows display-mode fallback when virtio-gpu ignores `EVENT_DISPLAY`.
//!
//! Uses `ChangeDisplaySettingsExW` (Win32). TODO(verify): virtio-win 0.1.302
//! ARM64 `viogpudo` may already apply the EDID mode; this path is the agent
//! `SetResolution` fallback.

/// `DM_PELSWIDTH` (wingdi.h).
#[cfg_attr(not(windows), allow(dead_code))]
pub const DM_PELSWIDTH: u32 = 0x0008_0000;
/// `DM_PELSHEIGHT` (wingdi.h).
#[cfg_attr(not(windows), allow(dead_code))]
pub const DM_PELSHEIGHT: u32 = 0x0010_0000;
/// `CDS_UPDATEREGISTRY`.
#[cfg(windows)]
const CDS_UPDATEREGISTRY: u32 = 0x0000_0001;
/// `DISP_CHANGE_SUCCESSFUL`.
#[cfg(windows)]
const DISP_CHANGE_SUCCESSFUL: i32 = 0;

/// Fields `ChangeDisplaySettingsExW` needs for a width×height mode.
#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(width, height)
)]
#[cfg_attr(not(windows), allow(dead_code))]
pub fn mode_fields(width: u32, height: u32) -> (u32, u32, u32) {
    (DM_PELSWIDTH | DM_PELSHEIGHT, width, height)
}

/// Apply `width`×`height` on the default display.
#[cfg(windows)]
#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(width, height)
)]
pub fn apply(width: u32, height: u32) -> Result<(), crate::AgentError> {
    use windows_sys::Win32::Graphics::Gdi::{ChangeDisplaySettingsExW, DEVMODEW};

    let (fields, w, h) = mode_fields(width, height);
    // SAFETY: `DEVMODEW` is zeroed then `dmSize`/`dmFields`/pixels are set
    // before the API call; the pointer is only used for the duration of the call.
    let rc = unsafe {
        let mut mode = std::mem::zeroed::<DEVMODEW>();
        mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
        mode.dmFields = fields;
        mode.dmPelsWidth = w;
        mode.dmPelsHeight = h;
        ChangeDisplaySettingsExW(
            std::ptr::null(),
            &mode,
            0,
            CDS_UPDATEREGISTRY,
            std::ptr::null(),
        )
    };
    if rc != DISP_CHANGE_SUCCESSFUL {
        tracing::warn!(
            target: "ternvale::agent",
            width,
            height,
            rc,
            "ChangeDisplaySettingsExW failed (virtio-gpu EDID path may still apply the mode)"
        );
        return Err(crate::AgentError::Action {
            action: "ChangeDisplaySettingsExW",
            source: std::io::Error::from_raw_os_error(rc),
        });
    }
    tracing::info!(target: "ternvale::agent", width, height, "guest display mode applied");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mode_fields_set_pels_bits() {
        let (fields, w, h) = mode_fields(1920, 1080);
        assert_eq!((w, h), (1920, 1080));
        assert_eq!(fields, DM_PELSWIDTH | DM_PELSHEIGHT);
    }
}
