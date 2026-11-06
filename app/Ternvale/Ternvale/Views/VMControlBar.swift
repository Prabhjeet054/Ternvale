import SwiftUI

struct VMControlBar: View {
    @AppStorage(AppSettings.seamlessWindowsKey) private var seamlessWindows = false
    let vm: VMConfig?
    let state: String
    let phase: VMRuntimePhase
    let crashSummary: String?
    let isRunning: Bool
    let isBusy: Bool
    let statusMessage: String?
    let toolsOutdatedMessage: String?
    let onStart: () -> Void
    let onStop: () -> Void
    let onPause: () -> Void
    let onResume: () -> Void
    let onSeamlessDisabled: () -> Void
    let onSendFeedback: () -> Void
    let onSettings: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(spacing: 12) {
                if let vm {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(vm.name)
                            .font(.title2)
                        Text(state)
                            .font(.subheadline)
                            .foregroundStyle(colorForPhase)
                    }
                } else {
                    Text("Select a VM")
                        .foregroundStyle(.secondary)
                }
                Spacer()
                Button("Start", action: onStart)
                    .disabled(vm == nil || isBusy || isRunning)
                Button("Stop", action: onStop)
                    .disabled(vm == nil || isBusy || !isRunning)
                Button("Pause", action: onPause)
                    .disabled(vm == nil || isBusy || phase != .running)
                Button("Resume", action: onResume)
                    .disabled(vm == nil || isBusy || phase != .paused)
                Button("Settings…", action: onSettings)
                    .disabled(vm == nil || isBusy || isRunning)
                    .help("Edit virtio-net backend and vmnet mode (shared / host-only / bridged).")
                Button("Send feedback…", action: onSendFeedback)
                    .disabled(vm == nil || isBusy)
                    .help("Zip this VM's crash bundle (Step 30) and open a mail draft. Nothing is uploaded.")
                Toggle("Seamless windows (experimental)", isOn: $seamlessWindows)
                    .toggleStyle(.switch)
                    .disabled(isBusy)
                    .help("One macOS window per guest app (Linux + wmctrl). Turn off while running to restore the embedded display; turn on before Start.")
                    .onChange(of: seamlessWindows) { _, enabled in
                        if !enabled {
                            onSeamlessDisabled()
                        }
                    }
            }
            if phase == .crashed, let crashSummary {
                GroupBox("Crash summary") {
                    Text(crashSummary)
                        .font(.caption)
                        .foregroundStyle(.primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
            }
            if let toolsOutdatedMessage, !toolsOutdatedMessage.isEmpty {
                Text(toolsOutdatedMessage)
                    .font(.caption)
                    .foregroundStyle(.primary)
                    .padding(8)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .background(.yellow.opacity(0.35))
            }
            if let statusMessage, !statusMessage.isEmpty {
                Text(statusMessage)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding()
    }

    private var colorForPhase: Color {
        switch phase {
        case .running: .green
        case .paused: .orange
        case .starting, .stopping: .secondary
        case .crashed: .red
        case .stopped: .secondary
        }
    }
}
