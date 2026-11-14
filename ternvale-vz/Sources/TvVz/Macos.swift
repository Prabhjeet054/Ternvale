import Foundation
import Virtualization

typealias tv_vz_progress_fn = @convention(c) (
    UnsafeMutableRawPointer?, UInt64, UInt64, UInt32
) -> Void

private func cString(_ ptr: UnsafePointer<CChar>?) -> String {
    guard let ptr else { return "" }
    return String(cString: ptr)
}

private func kv(_ pairs: [String: String]) -> String {
    pairs.map { "\($0.key)=\($0.value.replacingOccurrences(of: "\n", with: " "))" }
        .sorted()
        .joined(separator: "\n")
}

private func copyOut(_ text: String, _ out: UnsafeMutablePointer<CChar>?, _ len: Int) {
    guard let out, len > 0 else { return }
    let bytes = Array(text.utf8.prefix(len - 1)) + [0]
    for (i, b) in bytes.enumerated() {
        out[i] = CChar(bitPattern: b)
    }
}

private func hexEncode(_ data: Data) -> String {
    data.map { String(format: "%02x", $0) }.joined()
}

private func hexDecode(_ s: String) -> Data? {
    let chars = Array(s)
    guard chars.count.isMultiple(of: 2), !chars.isEmpty else { return nil }
    var data = Data(capacity: chars.count / 2)
    var i = 0
    while i < chars.count {
        let byte = String(chars[i]) + String(chars[i + 1])
        guard let b = UInt8(byte, radix: 16) else { return nil }
        data.append(b)
        i += 2
    }
    return data
}

/// `VZMacOSRestoreImage.fetchLatestSupported` / `loadFromURL` — macOS 12.0.
@_cdecl("tv_vz_macos_inspect")
func tv_vz_macos_inspect(
    _ ipsw: UnsafePointer<CChar>?,
    _ out: UnsafeMutablePointer<CChar>?,
    _ outLen: Int,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    let path = cString(ipsw)
    let sem = DispatchSemaphore(value: 0)
    var image: VZMacOSRestoreImage?
    var fail: String?
    if path.isEmpty {
        VZMacOSRestoreImage.fetchLatestSupported { result in
            switch result {
            case .success(let img): image = img
            case .failure(let error): fail = error.localizedDescription
            }
            sem.signal()
        }
    } else {
        VZMacOSRestoreImage.load(from: URL(fileURLWithPath: path)) { result in
            switch result {
            case .success(let img): image = img
            case .failure(let error): fail = error.localizedDescription
            }
            sem.signal()
        }
    }
    sem.wait()
    if let fail {
        tvCopyError(fail, err, errLen)
        return -1
    }
    guard let image else {
        tvCopyError("no restore image", err, errLen)
        return -1
    }
    // mostFeaturefulSupportedConfiguration — macOS 12.0.
    // (Apple's name; the step text said mostRecentSupportedConfiguration.)
    let req = image.mostFeaturefulSupportedConfiguration
    let hw = req?.hardwareModel
    let hwOk = hw?.isSupported ?? false
    let identifier = VZMacMachineIdentifier()
    let length = headLength(image.url) ?? 0
    var reason = ""
    if req == nil {
        reason = "mostFeaturefulSupportedConfiguration is nil; this Mac cannot run this restore image"
    } else if !hwOk {
        reason = "hardwareModel.isSupported is false; this Mac cannot run this restore image"
    }
    let os = image.operatingSystemVersion
    let osText = "\(os.majorVersion).\(os.minorVersion).\(os.patchVersion)"
    var fields: [String: String] = [:]
    fields["url"] = image.url.absoluteString
    fields["build"] = image.buildVersion
    fields["os"] = osText
    fields["min_cpus"] = req.map { String($0.minimumSupportedCPUCount) } ?? "0"
    fields["min_ram_bytes"] = req.map { String($0.minimumSupportedMemorySize) } ?? "0"
    fields["content_length"] = String(length)
    fields["supported"] = (req != nil && hwOk) ? "1" : "0"
    fields["hardware_supported"] = hwOk ? "1" : "0"
    fields["hw"] = hw.map { hexEncode($0.dataRepresentation) } ?? ""
    fields["id"] = hexEncode(identifier.dataRepresentation)
    fields["reason"] = reason
    let blob = kv(fields)
    copyOut(blob, out, outLen)
    TvLog.emit(.info, "macos restore build=\(image.buildVersion) os=\(osText) supported=\(req != nil && hwOk)")
    return 0
}

private func headLength(_ url: URL) -> UInt64? {
    var req = URLRequest(url: url)
    req.httpMethod = "HEAD"
    let sem = DispatchSemaphore(value: 0)
    var n: UInt64?
    URLSession.shared.dataTask(with: req) { _, resp, _ in
        if let http = resp as? HTTPURLResponse,
           let value = http.value(forHTTPHeaderField: "Content-Length"),
           let parsed = UInt64(value)
        {
            n = parsed
        }
        sem.signal()
    }.resume()
    sem.wait()
    return n
}

/// `VZMacAuxiliaryStorage.init(creatingStorageAt:hardwareModel:)` — macOS 12.0.
@_cdecl("tv_vz_macos_create_aux")
func tv_vz_macos_create_aux(
    _ aux: UnsafePointer<CChar>?,
    _ hwHex: UnsafePointer<CChar>?,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    let path = cString(aux)
    guard let hwData = hexDecode(cString(hwHex)),
          let model = VZMacHardwareModel(dataRepresentation: hwData)
    else {
        tvCopyError("invalid hardwareModel", err, errLen)
        return -1
    }
    do {
        if FileManager.default.fileExists(atPath: path) {
            return 0
        }
        _ = try VZMacAuxiliaryStorage(
            creatingStorageAt: URL(fileURLWithPath: path),
            hardwareModel: model
        )
        TvLog.emit(.info, "macos auxiliary storage path=\(path)")
        return 0
    } catch {
        tvCopyError(error.localizedDescription, err, errLen)
        return -1
    }
}

/// `VZMacOSInstaller` — macOS 13.0. Progress is `NSProgress` / `Progress`.
@_cdecl("tv_vz_macos_install")
func tv_vz_macos_install(
    _ ipsw: UnsafePointer<CChar>?,
    _ disk: UnsafePointer<CChar>?,
    _ aux: UnsafePointer<CChar>?,
    _ hwHex: UnsafePointer<CChar>?,
    _ idHex: UnsafePointer<CChar>?,
    _ cpus: UInt32,
    _ ramBytes: UInt64,
    _ cb: tv_vz_progress_fn?,
    _ ctx: UnsafeMutableRawPointer?,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    let ipswPath = cString(ipsw)
    let semImg = DispatchSemaphore(value: 0)
    var image: VZMacOSRestoreImage?
    var fail: String?
    VZMacOSRestoreImage.load(from: URL(fileURLWithPath: ipswPath)) { result in
        switch result {
        case .success(let img): image = img
        case .failure(let error): fail = error.localizedDescription
        }
        semImg.signal()
    }
    semImg.wait()
    if let fail {
        tvCopyError(fail, err, errLen)
        return -1
    }
    guard let image else {
        tvCopyError("restore image missing", err, errLen)
        return -1
    }
    _ = image.buildVersion
    do {
        let config = try macInstallConfig(
            disk: cString(disk),
            aux: cString(aux),
            hwHex: cString(hwHex),
            idHex: cString(idHex),
            cpus: cpus,
            ramBytes: ramBytes
        )
        let queue = DispatchQueue(label: "ternvale.vz.macos.install")
        let box = InstallerBox()
        let sem = DispatchSemaphore(value: 0)
        var installFail: String?
        // VZMacOSInstaller.init must run on the VM queue (dispatch_assert_queue).
        queue.async {
            // VZVirtualMachine.init(configuration:queue:) — macOS 12.0.
            let vm = VZVirtualMachine(configuration: config, queue: queue)
            box.vm = vm
            // VZMacOSInstaller.init(virtualMachine:restoreImageURL:) — macOS 13.0.
            let installer = VZMacOSInstaller(
                virtualMachine: vm,
                restoringFromImageAt: URL(fileURLWithPath: ipswPath)
            )
            box.installer = installer
            box.obs = installer.progress.observe(\.fractionCompleted) { progress, _ in
                let permille = UInt32((progress.fractionCompleted * 1000.0).rounded(.down))
                cb?(ctx, UInt64(progress.completedUnitCount), UInt64(progress.totalUnitCount), min(permille, 1000))
            }
            installer.install { result in
                if case .failure(let error) = result {
                    installFail = error.localizedDescription
                }
                sem.signal()
            }
        }
        sem.wait()
        box.obs?.invalidate()
        if let installFail {
            tvCopyError(installFail, err, errLen)
            return -1
        }
        cb?(ctx, 1, 1, 1000)
        TvLog.emit(.info, "macos installer completed")
        return 0
    } catch {
        tvCopyError(error.localizedDescription, err, errLen)
        return -1
    }
}

func applyMacPlatform(
    _ config: VZVirtualMachineConfiguration,
    hwHex: String,
    idHex: String,
    aux: String
) throws {
    guard let hwData = hexDecode(hwHex),
          let idData = hexDecode(idHex),
          let model = VZMacHardwareModel(dataRepresentation: hwData),
          let ident = VZMacMachineIdentifier(dataRepresentation: idData)
    else {
        throw NSError(
            domain: "ternvale.vz",
            code: 1,
            userInfo: [NSLocalizedDescriptionKey: "corrupt macos platform identity; the VM cannot boot"]
        )
    }
    if !model.isSupported {
        throw NSError(
            domain: "ternvale.vz",
            code: 1,
            userInfo: [NSLocalizedDescriptionKey: "hardwareModel.isSupported is false on this Mac"]
        )
    }
    // VZMacPlatformConfiguration — macOS 12.0.
    let platform = VZMacPlatformConfiguration()
    platform.hardwareModel = model
    platform.machineIdentifier = ident
    // VZMacAuxiliaryStorage.init(contentsOf:) — macOS 12.0.
    platform.auxiliaryStorage = VZMacAuxiliaryStorage(contentsOf: URL(fileURLWithPath: aux))
    config.platform = platform
    // VZMacOSBootLoader — macOS 12.0.
    config.bootLoader = VZMacOSBootLoader()
    attachMacInput(config)
}

private func macInstallConfig(
    disk: String,
    aux: String,
    hwHex: String,
    idHex: String,
    cpus: UInt32,
    ramBytes: UInt64
) throws -> VZVirtualMachineConfiguration {
    let config = VZVirtualMachineConfiguration()
    config.cpuCount = Int(cpus)
    config.memorySize = ramBytes
    try applyMacPlatform(config, hwHex: hwHex, idHex: idHex, aux: aux)
    let attachment = try VZDiskImageStorageDeviceAttachment(
        url: URL(fileURLWithPath: disk),
        readOnly: false,
        cachingMode: .automatic,
        synchronizationMode: .fsync
    )
    config.storageDevices = [VZVirtioBlockDeviceConfiguration(attachment: attachment)]
    let net = VZVirtioNetworkDeviceConfiguration()
    net.attachment = VZNATNetworkDeviceAttachment()
    config.networkDevices = [net]
    config.entropyDevices = [VZVirtioEntropyDeviceConfiguration()]
    try config.validate()
    return config
}

/// Keeps the installer VM alive until `install` finishes.
private final class InstallerBox {
    var vm: VZVirtualMachine?
    var installer: VZMacOSInstaller?
    var obs: NSKeyValueObservation?
}

func attachMacInput(_ config: VZVirtualMachineConfiguration) {
    // VZMacGraphicsDeviceConfiguration — macOS 12.0. Full display tuning is Step 94.
    let graphics = VZMacGraphicsDeviceConfiguration()
    let display = VZMacGraphicsDisplayConfiguration(widthInPixels: 1920, heightInPixels: 1080, pixelsPerInch: 80)
    graphics.displays = [display]
    config.graphicsDevices = [graphics]
    if #available(macOS 14.0, *) {
        // VZMacKeyboardConfiguration / VZMacTrackpadConfiguration — macOS 14.0.
        config.keyboards = [VZMacKeyboardConfiguration()]
        config.pointingDevices = [VZMacTrackpadConfiguration()]
    } else {
        config.keyboards = [VZUSBKeyboardConfiguration()]
        config.pointingDevices = [VZUSBScreenCoordinatePointingDeviceConfiguration()]
    }
}
