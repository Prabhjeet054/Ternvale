import SwiftUI

struct CreateVMView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var kernelPath = ""

    let onCreate: (String, String?) -> Void

    var body: some View {
        Form {
            TextField("Name", text: $name)
            TextField("Kernel path (optional)", text: $kernelPath)
            Text("Creates ~/Library/Application Support/Ternvale/vms/<name>/config.toml")
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .formStyle(.grouped)
        .frame(width: 420, height: 200)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Create") {
                    onCreate(name, kernelPath.nilIfEmpty)
                    dismiss()
                }
                .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
            }
        }
        .navigationTitle("New VM")
    }
}

private extension String {
    var nilIfEmpty: String? {
        let t = trimmingCharacters(in: .whitespacesAndNewlines)
        return t.isEmpty ? nil : t
    }
}
