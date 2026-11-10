import SwiftUI

/// Lists every file and field that would be POSTed. Send is a second explicit step.
struct CrashReportPreviewView: View {
    let payload: CrashReportPayload
    let optedIn: Bool
    var onSend: () -> Void
    var onCancel: () -> Void
    @State private var sending = false
    @State private var error: String?

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Crash report preview")
                .font(.title2)
            Text("Nothing is sent until you click Send. Guest serial output and guest files are excluded.")
                .font(.caption)
                .foregroundStyle(.secondary)
            List(payload.items) { item in
                VStack(alignment: .leading, spacing: 4) {
                    HStack {
                        Text(item.included ? "Include" : "Omit")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(item.included ? .green : .secondary)
                        Text(item.kind)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Text(item.name)
                            .font(.body.monospaced())
                        if let bytes = item.bytes {
                            Spacer()
                            Text("\(bytes) bytes")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    if let reason = item.reason {
                        Text(reason).font(.caption).foregroundStyle(.orange)
                    }
                    if let preview = item.redactedPreview, !preview.isEmpty {
                        Text(preview)
                            .font(.caption.monospaced())
                            .lineLimit(4)
                    }
                }
                .padding(.vertical, 2)
            }
            if !optedIn {
                Text("Crash reporting is off. Enable it in Preferences to send this report.")
                    .foregroundStyle(.orange)
            }
            if let error {
                Text(error).foregroundStyle(.red)
            }
            HStack {
                Button("Don't Send", action: onCancel)
                Spacer()
                Button("Send") {
                    sending = true
                    onSend()
                }
                .disabled(!optedIn || sending || payload.includedItems.isEmpty)
                .keyboardShortcut(.defaultAction)
            }
        }
        .padding()
        .frame(minWidth: 520, minHeight: 360)
    }
}
