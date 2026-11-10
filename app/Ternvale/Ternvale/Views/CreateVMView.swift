import SwiftUI

struct NewVMRequest {
    var name: String
    var kernelPath: String?
    var windowsUnattended: Bool
    var windowsIso: String
    var password: String
    var productKey: String
}

struct CreateVMView: View {
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""
    @State private var kernelPath = ""
    @State private var windowsUnattended = false
    @State private var windowsIso = ""
    @State private var password = ""
    @State private var productKey = ""

    let onCreate: (NewVMRequest) -> Void

    var body: some View {
        Form {
            TextField("Name", text: $name)
            Toggle("Windows 11 (unattended)", isOn: $windowsUnattended)
            if windowsUnattended {
                TextField("Windows ISO path", text: $windowsIso)
                SecureField("Account password (Keychain)", text: $password)
                TextField("Product key (optional)", text: $productKey)
                Text("You supply the Windows ISO and licence. Ternvale never generates keys. Set TERNVALE_FIRMWARE (or --firmware) and build a tools ISO first; create is unattended-only.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            } else {
                TextField("Kernel path (optional)", text: $kernelPath)
                Text("Writes ~/Library/Application Support/Ternvale/vms/<name>/config.toml. You still need a real ARM64 kernel or firmware, and `ternvale create-disk` on disks/disk0.img before Start.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .formStyle(.grouped)
        .frame(width: 460, height: windowsUnattended ? 360 : 240)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Create") {
                    onCreate(
                        NewVMRequest(
                            name: name,
                            kernelPath: kernelPath.nilIfEmpty,
                            windowsUnattended: windowsUnattended,
                            windowsIso: windowsIso,
                            password: password,
                            productKey: productKey
                        )
                    )
                    dismiss()
                }
                .disabled(!canCreate)
            }
        }
        .navigationTitle("New VM")
    }

    private var canCreate: Bool {
        let n = name.trimmingCharacters(in: .whitespaces)
        if n.isEmpty { return false }
        if windowsUnattended {
            return !windowsIso.trimmingCharacters(in: .whitespaces).isEmpty
                && !password.isEmpty
        }
        return true
    }
}

private extension String {
    var nilIfEmpty: String? {
        let t = trimmingCharacters(in: .whitespacesAndNewlines)
        return t.isEmpty ? nil : t
    }
}
