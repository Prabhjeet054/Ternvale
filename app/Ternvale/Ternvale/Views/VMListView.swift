import SwiftUI

struct VMListView: View {
    let vms: [VMConfig]
    @Binding var selection: VMConfig?
    let liveState: String
    let onRefresh: () -> Void
    let onCreate: () -> Void

    var body: some View {
        List(selection: $selection) {
            Section("Virtual machines") {
                if vms.isEmpty {
                    Text("No VMs in ~/Library/Application Support/Ternvale/vms/")
                        .foregroundStyle(.secondary)
                        .font(.callout)
                }
                ForEach(vms) { vm in
                    VStack(alignment: .leading, spacing: 2) {
                        Text(vm.name)
                            .font(.headline)
                        Text("\(vm.cpus) CPU · \(vm.ramMiB) MiB")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    .tag(vm)
                }
            }
        }
        .toolbar {
            ToolbarItem(placement: .primaryAction) {
                Button("New VM", action: onCreate)
            }
            ToolbarItem(placement: .automatic) {
                Button("Refresh", action: onRefresh)
            }
            if let selection {
                ToolbarItem(placement: .status) {
                    Text(selection.name)
                }
                ToolbarItem(placement: .status) {
                    Text(liveState)
                        .foregroundStyle(.secondary)
                }
            }
        }
        .navigationTitle("Ternvale")
    }
}
