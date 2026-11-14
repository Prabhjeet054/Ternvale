import Foundation
import Virtualization

/// `VZLinuxRosettaDirectoryShare.availability` — macOS 13.0.
/// Raw values match `VZLinuxRosettaAvailability` in VZLinuxRosettaDirectoryShare.h.
@_cdecl("tv_vz_rosetta_availability")
func tv_vz_rosetta_availability() -> Int32 {
    if #available(macOS 13.0, *) {
        let value = VZLinuxRosettaDirectoryShare.availability
        TvLog.emit(.debug, "rosetta availability=\(value.rawValue)")
        return Int32(value.rawValue)
    }
    TvLog.emit(.info, "rosetta availability=0 (macOS < 13)")
    return 0
}

/// `installRosetta` — macOS 13.0. Caller (Rust) must already have typed consent.
@_cdecl("tv_vz_rosetta_install")
func tv_vz_rosetta_install(
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    if #available(macOS 13.0, *) {
        let sem = DispatchSemaphore(value: 0)
        var fail: String?
        // Completion may run on an arbitrary queue (Apple header).
        VZLinuxRosettaDirectoryShare.installRosetta { error in
            if let error {
                fail = error.localizedDescription
            }
            sem.signal()
        }
        sem.wait()
        if let fail {
            TvLog.emit(.error, "rosetta installRosetta failed \(fail)")
            tvCopyError(fail, err, errLen)
            return -1
        }
        TvLog.emit(.info, "rosetta installRosetta completed")
        return 0
    }
    tvCopyError("Rosetta for Linux needs macOS 13+", err, errLen)
    return -1
}

/// Attach `VZLinuxRosettaDirectoryShare` as virtiofs tag `rosetta`.
func attachLinuxRosettaIfRequested(
    _ config: VZVirtualMachineConfiguration,
    devices: TvVzDevices
) throws {
    guard devices.rosetta else { return }
    guard #available(macOS 13.0, *) else {
        throw NSError(
            domain: "ternvale.vz",
            code: 1,
            userInfo: [NSLocalizedDescriptionKey: "Rosetta for Linux needs macOS 13+"]
        )
    }
    let tag = devices.rosettaTag.isEmpty ? "rosetta" : devices.rosettaTag
    try VZVirtioFileSystemDeviceConfiguration.validateTag(tag)
    let share = try VZLinuxRosettaDirectoryShare()
    if devices.rosettaAot {
        if #available(macOS 14.0, *) {
            // VZLinuxRosettaDirectoryShare.CachingOptions.abstractSocket — macOS 14.0
            // (Virtualization.swiftmodule). Optional: skip if the host rejects it.
            do {
                try share.setCachingOptions(.abstractSocket(devices.rosettaAotName))
                TvLog.emit(
                    .info,
                    "rosetta AOT abstractSocket name=\(devices.rosettaAotName)"
                )
            } catch {
                TvLog.emit(
                    .warn,
                    "rosetta AOT cache skipped \(error.localizedDescription)"
                )
            }
        } else {
            TvLog.emit(.info, "rosetta AOT skipped (needs macOS 14+)")
        }
    }
    let fs = VZVirtioFileSystemDeviceConfiguration(tag: tag)
    fs.share = share
    var sharing = config.directorySharingDevices
    sharing.append(fs)
    config.directorySharingDevices = sharing
    TvLog.emit(.info, "rosetta virtiofs tag=\(tag) share=VZLinuxRosettaDirectoryShare")
}
