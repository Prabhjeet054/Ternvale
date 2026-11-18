import Foundation
import Virtualization

/// `VZGenericPlatformConfiguration.isNestedVirtualizationSupported` — macOS 15.0.
/// Returns 1 if nested virt is available, 0 if the API exists but is false,
/// -1 if the process is older than macOS 15 (property not present).
@_cdecl("tv_vz_nested_supported")
func tv_vz_nested_supported() -> Int32 {
    if #available(macOS 15.0, *) {
        // Class property; minimum macOS 15.0 (VZGenericPlatformConfiguration.h).
        let supported = VZGenericPlatformConfiguration.isNestedVirtualizationSupported
        TvLog.emit(.info, "vz nestedVirtualizationSupported=\(supported)")
        return supported ? 1 : 0
    }
    TvLog.emit(.info, "vz nestedVirtualizationSupported unavailable (macOS < 15)")
    return -1
}
