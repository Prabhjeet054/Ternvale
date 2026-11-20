import SwiftUI

/// Per-VM passthrough: mic, camera, USB, shares, clipboard. Mic/camera default off.
struct PrivacySettingsView: View {
    let configURL: URL
    var live: PrivacyLiveState?
    let onSave: () -> Void

    @State private var microphone = false
    @State private var camera = false
    @State private var usbEnabled = true
    @State private var clipboard = false
    @State private var clipboardRich = false
    @State private var clipboardDnd = false
    @State private var launchers = false
    @State private var openInGuest = false
    @State private var openOnHost = false
    @State private var shares: [(tag: String, hostPath: String, readOnly: Bool)] = []
    @State private var error: String?
    @State private var loaded = false

    var body: some View {
        Form {
            Section("Microphone") {
                Toggle("Allow microphone into this VM", isOn: $microphone)
                Text(
                    "Off by default. macOS will ask for microphone access the first time capture starts (NSMicrophoneUsageDescription). Audio is never recorded or written to logs."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                if let live {
                    Text("Live: \(live.microphone)\(reasonSuffix(live.microphoneReason))")
                        .font(.caption)
                }
            }
            Section("Camera") {
                Toggle("Allow camera into this VM", isOn: $camera)
                Text(
                    "Off by default. Presented through the guest agent (not USB Video Class). xHCI has no isochronous transfers, so a virtual UVC device is not attached. Video is never recorded or logged."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
                if let live {
                    Text(
                        "Live: \(live.camera) (\(live.cameraTransport))\(reasonSuffix(live.cameraReason))"
                    )
                    .font(.caption)
                }
            }
            Section("USB") {
                LabeledContent("xHCI controller") {
                    Text(usbEnabled ? "On" : "Off")
                }
                Text("Edit [usb] in config.toml to change the controller or passthrough devices.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("Shared folders") {
                if shares.isEmpty {
                    Text("None").foregroundStyle(.secondary)
                } else {
                    ForEach(Array(shares.enumerated()), id: \.offset) { _, share in
                        VStack(alignment: .leading) {
                            Text(share.tag.isEmpty ? "(no tag)" : share.tag)
                            Text(share.hostPath)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                            if share.readOnly {
                                Text("Read only").font(.caption2)
                            }
                        }
                    }
                }
            }
            Section("Clipboard") {
                LabeledContent("Guest agent clipboard") {
                    Text(clipboard ? "On" : "Off")
                }
                Toggle("Rich clipboard (PNG, RTF, HTML, files)", isOn: $clipboardRich)
                Toggle("Drag and drop with Finder", isOn: $clipboardDnd)
                Text(
                    "Off by default. Transfers larger than clipboard.confirm_mib ask first. Incoming names are sanitized; existing files are never overwritten silently."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            Section("Guest apps") {
                Toggle("Start Menu launchers in ~/Applications/Ternvale Apps", isOn: $launchers)
                Toggle("Open Mac files in guest apps (share path only)", isOn: $openInGuest)
                Toggle("Open guest links/files with macOS default apps", isOn: $openOnHost)
                Text(
                    "Off by default, per VM. URLs are limited to http, https, and mailto unless you add a scheme. file: is never allowed. The first use of each association asks for confirmation. Uninstall launchers with ternvale apps uninstall-stubs."
                )
                .font(.caption)
                .foregroundStyle(.secondary)
            }
            if let error {
                Text(error).foregroundStyle(.red).font(.caption)
            }
            Button("Save privacy") { save() }
        }
        .formStyle(.grouped)
        .onAppear(perform: load)
    }

    private func reasonSuffix(_ reason: String?) -> String {
        guard let reason, !reason.isEmpty else { return "" }
        return " — \(reason)"
    }

    private func load() {
        guard !loaded else { return }
        loaded = true
        do {
            let text = try String(contentsOf: configURL, encoding: .utf8)
            microphone = VMConfigParser.parseAudioInput(in: text)
            camera = VMConfigParser.parseCameraEnabled(in: text)
            usbEnabled = VMConfigParser.parseUsbEnabled(in: text)
            clipboard = VMConfigParser.parseClipboardAgent(in: text)
            clipboardRich = VMConfigParser.parseClipboardRich(in: text)
            clipboardDnd = VMConfigParser.parseClipboardDnd(in: text)
            launchers = VMConfigParser.parseAgentLaunchers(in: text)
            openInGuest = VMConfigParser.parseAgentOpenInGuest(in: text)
            openOnHost = VMConfigParser.parseAgentOpenOnHost(in: text)
            shares = VMConfigParser.parseShares(in: text)
        } catch {
            self.error = error.localizedDescription
        }
    }

    private func save() {
        do {
            var text = try String(contentsOf: configURL, encoding: .utf8)
            text = VMConfigParser.replacingAudioInput(in: text, enabled: microphone)
            text = VMConfigParser.replacingCameraEnabled(in: text, enabled: camera)
            text = VMConfigParser.replacingClipboardRich(in: text, enabled: clipboardRich)
            text = VMConfigParser.replacingClipboardDnd(in: text, enabled: clipboardDnd)
            text = VMConfigParser.replacingAgentLaunchers(in: text, enabled: launchers)
            text = VMConfigParser.replacingAgentOpenInGuest(in: text, enabled: openInGuest)
            text = VMConfigParser.replacingAgentOpenOnHost(in: text, enabled: openOnHost)
            try text.write(to: configURL, atomically: true, encoding: .utf8)
            onSave()
        } catch {
            self.error = error.localizedDescription
        }
    }
}

struct PrivacyLiveState: Equatable {
    var microphone: String
    var microphoneReason: String?
    var camera: String
    var cameraReason: String?
    var cameraTransport: String
}
