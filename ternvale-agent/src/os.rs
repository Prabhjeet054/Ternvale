//! The `os` string sent in `Hello`.

/// `"Windows build <N> aarch64"` on Windows, `"<kernel> <release> <arch>"` on
/// Linux, else `"<os> <arch>"`.
#[tracing::instrument(level = "debug", target = "ternvale::agent", skip_all)]
pub fn os_description() -> String {
    let arch = std::env::consts::ARCH;
    let description = windows_os(arch).unwrap_or_else(|| linux_or_fallback(arch));
    let mut description = description;
    description.truncate(ternvale_agent_proto::MAX_NAME);
    tracing::debug!(target: "ternvale::agent", os = %description, "hello os string");
    description
}

/// Format used in `Hello.os` and `ternvale status`.
#[cfg_attr(not(any(windows, test)), allow(dead_code))]
#[tracing::instrument(
    level = "debug",
    target = "ternvale::agent",
    skip_all,
    fields(build, arch)
)]
pub fn format_windows_os(major: u32, minor: u32, build: u32, arch: &str) -> String {
    format!("Windows {major}.{minor} build {build} {arch}")
}

fn linux_or_fallback(arch: &str) -> String {
    let read = |path: &str| {
        std::fs::read_to_string(path)
            .map(|text| text.trim().to_string())
            .map_err(|error| {
                tracing::debug!(target: "ternvale::agent", path, error = %error, "kernel info unavailable");
            })
            .ok()
            .filter(|text| !text.is_empty())
    };
    match (
        read("/proc/sys/kernel/ostype"),
        read("/proc/sys/kernel/osrelease"),
    ) {
        (Some(kind), Some(release)) => format!("{kind} {release} {arch}"),
        _ => format!("{} {arch}", std::env::consts::OS),
    }
}

fn windows_os(arch: &str) -> Option<String> {
    #[cfg(windows)]
    {
        return Some(windows_rtl_os(arch));
    }
    #[cfg(not(windows))]
    {
        let _ = arch;
        None
    }
}

#[cfg(windows)]
fn windows_rtl_os(arch: &str) -> String {
    #[repr(C)]
    struct OsVersionInfoW {
        size: u32,
        major: u32,
        minor: u32,
        build: u32,
        platform: u32,
        csd: [u16; 128],
    }
    extern "system" {
        fn RtlGetVersion(info: *mut OsVersionInfoW) -> i32;
    }
    let mut info = OsVersionInfoW {
        size: std::mem::size_of::<OsVersionInfoW>() as u32,
        major: 0,
        minor: 0,
        build: 0,
        platform: 0,
        csd: [0; 128],
    };
    // SAFETY: RtlGetVersion writes the sized OSVERSIONINFOW we pass.
    let status = unsafe { RtlGetVersion(&mut info) };
    if status != 0 {
        tracing::warn!(target: "ternvale::agent", status, "RtlGetVersion failed");
        return format!("Windows {arch}");
    }
    format_windows_os(info.major, info.minor, info.build, arch)
}

#[cfg(test)]
mod tests {
    use super::format_windows_os;

    #[test]
    fn windows_hello_includes_build_and_arch() {
        let text = format_windows_os(10, 0, 22621, "aarch64");
        assert_eq!(text, "Windows 10.0 build 22621 aarch64");
    }
}
