import Foundation
import Virtualization

/// Parsed extras for macOS (and Linux SPICE) device attach.
struct TvVzDevices {
    var width: Int = 1920
    var height: Int = 1080
    var ppi: Int = 80
    var audioOut = true
    var audioIn = false
    var spice = false
    var showView = false
    var shares: [(tag: String, path: String, readOnly: Bool)] = []
}

func parseTvVzDevices(_ raw: String) -> TvVzDevices {
    var out = TvVzDevices()
    if raw.isEmpty { return out }
    for line in raw.split(separator: "\n", omittingEmptySubsequences: true) {
        let text = String(line)
        guard let eq = text.firstIndex(of: "=") else { continue }
        let key = String(text[..<eq])
        let value = String(text[text.index(after: eq)...])
        switch key {
        case "w": out.width = Int(value) ?? out.width
        case "h": out.height = Int(value) ?? out.height
        case "ppi": out.ppi = Int(value) ?? out.ppi
        case "audio_out": out.audioOut = value == "1"
        case "audio_in": out.audioIn = value == "1"
        case "spice": out.spice = value == "1"
        case "view": out.showView = value == "1"
        case "share":
            let parts = value.split(separator: "\t", omittingEmptySubsequences: false).map(String.init)
            if parts.count >= 3 {
                out.shares.append((parts[0], parts[1], parts[2] == "1"))
            }
        default:
            break
        }
    }
    out.width = max(640, min(out.width, 7680))
    out.height = max(480, min(out.height, 4320))
    out.ppi = max(72, min(out.ppi, 300))
    return out
}

/// Graphics, HID, sound, virtiofs, optional SPICE. NAT is attached in `buildConfig`.
func attachMacGuestDevices(_ config: VZVirtualMachineConfiguration, devices: TvVzDevices) throws {
    // VZMacGraphicsDeviceConfiguration / VZMacGraphicsDisplayConfiguration — macOS 12.0.
    let graphics = VZMacGraphicsDeviceConfiguration()
    let display = VZMacGraphicsDisplayConfiguration(
        widthInPixels: devices.width,
        heightInPixels: devices.height,
        pixelsPerInch: devices.ppi
    )
    graphics.displays = [display]
    config.graphicsDevices = [graphics]
    TvLog.emit(
        .info,
        "macos graphics \(devices.width)x\(devices.height)@\(devices.ppi)ppi"
    )

    if #available(macOS 14.0, *) {
        // VZMacKeyboardConfiguration / VZMacTrackpadConfiguration — macOS 14.0.
        config.keyboards = [VZMacKeyboardConfiguration()]
        config.pointingDevices = [VZMacTrackpadConfiguration()]
        TvLog.emit(.debug, "macos hid=mac-keyboard+trackpad")
    } else {
        // USB HID fallback — macOS 12.0.
        config.keyboards = [VZUSBKeyboardConfiguration()]
        config.pointingDevices = [VZUSBScreenCoordinatePointingDeviceConfiguration()]
        TvLog.emit(.debug, "macos hid=usb-keyboard+tablet (pre-14 fallback)")
    }

    if devices.audioOut || devices.audioIn {
        try attachVirtioSound(config, output: devices.audioOut, input: devices.audioIn)
    }

    if !devices.shares.isEmpty {
        try attachVirtiofs(config, shares: devices.shares, macosGuest: true)
    }

    if devices.spice {
        // VZSpiceAgentPortAttachment — macOS 13.0. Linux guests with spice-vdagent only.
        // macOS guests: Apple documents clipboard sharing for Linux; opening the
        // SPICE tty has been observed to abort a macOS guest (TODO(verify) later SLAs).
        TvLog.emit(
            .warn,
            "spice clipboard requested on a macOS guest; skipped (Linux spice-vdagent only)"
        )
    }
}

func attachLinuxSpiceIfRequested(_ config: VZVirtualMachineConfiguration, devices: TvVzDevices) {
    guard devices.spice else { return }
    if #available(macOS 13.0, *) {
        attachSpiceClipboard(config)
        TvLog.emit(.info, "linux spice clipboard port attached")
    }
}

/// `VZVirtioSoundDeviceConfiguration` — macOS 12.0.
func attachVirtioSound(_ config: VZVirtualMachineConfiguration, output: Bool, input: Bool) throws {
    let sound = VZVirtioSoundDeviceConfiguration()
    var streams: [VZVirtioSoundDeviceStreamConfiguration] = []
    if output {
        let stream = VZVirtioSoundDeviceOutputStreamConfiguration()
        stream.sink = VZHostAudioOutputStreamSink()
        streams.append(stream)
    }
    if input {
        // Microphone: opt-in `[audio] input = true`. Step 111 owns TCC UI.
        let stream = VZVirtioSoundDeviceInputStreamConfiguration()
        stream.source = VZHostAudioInputStreamSource()
        streams.append(stream)
        TvLog.emit(.info, "macos audio input enabled (host microphone; Step 111 permission model)")
    }
    sound.streams = streams
    config.audioDevices = [sound]
    TvLog.emit(.info, "virtio-sound output=\(output ? 1 : 0) input=\(input ? 1 : 0)")
}

/// `VZVirtioFileSystemDeviceConfiguration` + `VZSharedDirectory` — macOS 12.0;
/// automount tag — macOS 13.0.
func attachVirtiofs(
    _ config: VZVirtualMachineConfiguration,
    shares: [(tag: String, path: String, readOnly: Bool)],
    macosGuest: Bool
) throws {
    if macosGuest, #available(macOS 13.0, *) {
        let tag = VZVirtioFileSystemDeviceConfiguration.macOSGuestAutomountTag
        let fs = VZVirtioFileSystemDeviceConfiguration(tag: tag)
        if shares.count == 1 {
            let share = shares[0]
            let dir = VZSharedDirectory(
                url: URL(fileURLWithPath: share.path),
                readOnly: share.readOnly
            )
            fs.share = VZSingleDirectoryShare(directory: dir)
            TvLog.emit(
                .info,
                "macos virtiofs automount tag=\(tag) path=\(share.path) ro=\(share.readOnly ? 1 : 0) guest=/Volumes/My Shared Files"
            )
        } else {
            var map: [String: VZSharedDirectory] = [:]
            for share in shares {
                let name = share.tag.isEmpty ? "share" : share.tag
                map[name] = VZSharedDirectory(
                    url: URL(fileURLWithPath: share.path),
                    readOnly: share.readOnly
                )
            }
            fs.share = VZMultipleDirectoryShare(directories: map)
            TvLog.emit(
                .info,
                "macos virtiofs automount multi count=\(shares.count) guest=/Volumes/My Shared Files/<tag>"
            )
        }
        config.directorySharingDevices = [fs]
        return
    }
    guard let first = shares.first else { return }
    let fs = VZVirtioFileSystemDeviceConfiguration(tag: first.tag)
    let dir = VZSharedDirectory(
        url: URL(fileURLWithPath: first.path),
        readOnly: first.readOnly
    )
    fs.share = VZSingleDirectoryShare(directory: dir)
    config.directorySharingDevices = [fs]
    TvLog.emit(.info, "virtiofs tag=\(first.tag) path=\(first.path)")
}

/// `VZSpiceAgentPortAttachment` on a virtio console port — macOS 13.0.
func attachSpiceClipboard(_ config: VZVirtualMachineConfiguration) {
    let console = VZVirtioConsoleDeviceConfiguration()
    let port = VZVirtioConsolePortConfiguration()
    port.name = VZSpiceAgentPortAttachment.spiceAgentPortName
    port.isConsole = false
    let spice = VZSpiceAgentPortAttachment()
    spice.sharesClipboard = true
    port.attachment = spice
    console.ports[0] = port
    var consoles = config.consoleDevices
    consoles.append(console)
    config.consoleDevices = consoles
}
