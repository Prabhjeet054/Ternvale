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
