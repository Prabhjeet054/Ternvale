import SwiftUI

struct VMControlBar: View {
    let vm: VMConfig?
    let state: String
    let phase: VMRuntimePhase
    let crashSummary: String?
    let isRunning: Bool
    let isBusy: Bool
    let statusMessage: String?
    let onStart: () -> Void
    let onStop: () -> Void
    let onPause: () -> Void
    let onResume: () -> Void

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
            }
            if phase == .crashed, let crashSummary {
                GroupBox("Crash summary") {
                    Text(crashSummary)
                        .font(.caption)
                        .foregroundStyle(.primary)
                        .frame(maxWidth: .infinity, alignment: .leading)
                }
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
