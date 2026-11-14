import Foundation
import Virtualization

/// `saveMachineStateTo` / `restoreMachineStateFrom` — macOS 14.0 (WWDC23-10007).
/// The VM must be paused to save and stopped to restore. Files are hardware
/// encrypted to this Mac; they cannot be restored on another host.
@_cdecl("tv_vz_save")
func tv_vz_save(
    _ handle: Int32,
    _ path: UnsafePointer<CChar>?,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    guard let session = Registry.get(handle), let vm = session.vm else {
        tvCopyError("invalid vz handle", err, errLen)
        return -1
    }
    let dest = URL(fileURLWithPath: cSavePath(path))
    if #available(macOS 14.0, *) {
        if let config = session.configuration {
            do {
                // validateSaveRestoreSupport() — macOS 14.0. Fails if a
                // device (USB, some shares) cannot be snapshotted.
                try config.validateSaveRestoreSupport()
            } catch {
                tvCopyError(
                    "VZ save/restore not supported for this device set: \(error.localizedDescription)",
                    err,
                    errLen
                )
                TvLog.emit(.error, "vm=\(session.name) vz save validate \(error.localizedDescription)")
                return -1
            }
        }
        let sem = DispatchSemaphore(value: 0)
        var fail: String?
        session.queue.async {
            vm.saveMachineStateTo(url: dest) { error in
                if let error {
                    fail = error.localizedDescription
                    TvLog.emit(.error, "vm=\(session.name) vz save \(error.localizedDescription)")
                } else {
                    TvLog.emit(.info, "vm=\(session.name) vz saveMachineStateTo path=\(dest.path)")
                }
                sem.signal()
            }
        }
        sem.wait()
        if let fail {
            tvCopyError(fail, err, errLen)
            return -1
        }
        return 0
    } else {
        tvCopyError("saveMachineStateTo requires macOS 14.0 (this host is older)", err, errLen)
        return -1
    }
}

@_cdecl("tv_vz_restore")
func tv_vz_restore(
    _ handle: Int32,
    _ path: UnsafePointer<CChar>?,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    guard let session = Registry.get(handle), let vm = session.vm else {
        tvCopyError("invalid vz handle", err, errLen)
        return -1
    }
    let src = URL(fileURLWithPath: cSavePath(path))
    if #available(macOS 14.0, *) {
        let sem = DispatchSemaphore(value: 0)
        var fail: String?
        session.queue.async {
            vm.restoreMachineStateFrom(url: src) { error in
                if let error {
                    fail = error.localizedDescription
                    TvLog.emit(.error, "vm=\(session.name) vz restore \(error.localizedDescription)")
                } else {
                    TvLog.emit(.info, "vm=\(session.name) vz restoreMachineStateFrom path=\(src.path)")
                    session.emit(.paused, detail: "restore")
                }
                sem.signal()
            }
        }
        sem.wait()
        if let fail {
            tvCopyError(fail, err, errLen)
            return -1
        }
        return 0
    } else {
        tvCopyError("restoreMachineStateFrom requires macOS 14.0 (this host is older)", err, errLen)
        return -1
    }
}

/// Fresh `VZMacMachineIdentifier` for a clone (never reuse the source).
@_cdecl("tv_vz_macos_new_machine_id")
func tv_vz_macos_new_machine_id(
    _ out: UnsafeMutablePointer<CChar>?,
    _ outLen: Int,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    let identifier = VZMacMachineIdentifier()
    let hex = identifier.dataRepresentation.map { String(format: "%02x", $0) }.joined()
    guard let out, outLen > 1 else {
        tvCopyError("new machine id output buffer too small", err, errLen)
        return -1
    }
    let bytes = Array(hex.utf8.prefix(outLen - 1)) + [0]
    for (i, b) in bytes.enumerated() {
        out[i] = CChar(bitPattern: b)
    }
    TvLog.emit(.info, "macos clone new machineIdentifier bytes=\(identifier.dataRepresentation.count)")
    return 0
}

private func cSavePath(_ ptr: UnsafePointer<CChar>?) -> String {
    guard let ptr else { return "" }
    return String(cString: ptr)
}
