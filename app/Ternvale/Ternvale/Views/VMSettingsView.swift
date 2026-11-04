import SwiftUI

/// Per-VM NIC settings written back to `config.toml`.
struct VMSettingsView: View {
    let configURL: URL
    let initial: [NicConfig]
    let onSave: ([NicConfig]) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var nics: [NicConfig]
    @State private var bridges: [String] = []
    @State private var error: String?

    init(configURL: URL, initial: [NicConfig], onSave: @escaping ([NicConfig]) -> Void) {
        self.configURL = configURL
        self.initial = initial
        self.onSave = onSave
        _nics = State(initialValue: initial.isEmpty ? [NicConfig.user(id: 0)] : initial)
    }

    var body: some View {
        Form {
            ForEach($nics) { $nic in
                Section("NIC \(nic.id)") {
                    Picker("Backend", selection: $nic.backend) {
                        Text("User NAT").tag("user")
                        Text("Loopback").tag("loopback")
                        Text("vmnet").tag("vmnet")
                    }
                    if nic.backend == "vmnet" {
                        Picker("Mode", selection: $nic.mode) {
                            Text("Shared (NAT)").tag("shared")
                            Text("Host-only").tag("host-only")
                            Text("Bridged").tag("bridged")
                        }
                        if nic.mode == "host-only" {
                            TextField("Virtual network name", text: $nic.network)
                            Text("VMs that share this name can reach each other, not the Internet.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        if nic.mode == "bridged" {
                            if bridges.isEmpty {
                                TextField("Bridge interface", text: $nic.bridgeInterface)
                            } else {
                                Picker("Bridge interface", selection: $nic.bridgeInterface) {
                                    ForEach(bridges, id: \.self) { name in
                                        Text(name).tag(name)
                                    }
                                }
                            }
                            Text(VmnetHelperService.bridgedEntitlementMissing)
                                .font(.caption)
                                .foregroundStyle(.orange)
                        }
                    }
                }
            }
            Button("Add NIC") {
                nics.append(NicConfig.user(id: nics.count))
            }
            if let error {
                Text(error).foregroundStyle(.red).font(.caption)
            }
        }
        .formStyle(.grouped)
        .frame(minWidth: 480, minHeight: 360)
        .navigationTitle("VM network")
        .toolbar {
            ToolbarItem(placement: .cancellationAction) {
                Button("Cancel") { dismiss() }
            }
            ToolbarItem(placement: .confirmationAction) {
                Button("Save") { save() }
            }
        }
        .onAppear {
            bridges = VmnetHelperService.listBridgeableInterfaces(helperURL: nil)
            if let first = bridges.first {
                for i in nics.indices where nics[i].mode == "bridged" && nics[i].bridgeInterface.isEmpty {
                    nics[i].bridgeInterface = first
                }
            }
        }
    }

    private func save() {
        do {
            let text = try String(contentsOf: configURL, encoding: .utf8)
            let next = VMConfigParser.replacingNics(in: text, nics: nics)
            try next.write(to: configURL, atomically: true, encoding: .utf8)
            onSave(nics)
            dismiss()
        } catch {
            self.error = error.localizedDescription
        }
    }
}
