import SwiftUI

/// Toggle “Run x86 programs” plus one-click Step 97/98 tooling via `ternvale x86 plan|setup`.
struct X86SettingsSection: View {
    let configURL: URL

    @State private var enabled = false
    @State private var plan: X86Plan?
    @State private var busy = false
    @State private var message: String?

    var body: some View {
        Group {
            Toggle("Run x86 programs", isOn: $enabled)
                .onChange(of: enabled) { _, on in
                    persist(enabled: on)
                }
            if let plan {
                Text(plan.engineSummary)
                    .font(.body)
                if let reason = plan.unavailableReason, !plan.available {
                    Text(reason)
                        .font(.caption)
                        .foregroundStyle(.orange)
                } else if plan.available {
                    Text(plan.setupHint)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                if plan.setupKind != "none" {
                    Button(setupTitle(plan)) {
                        runSetup()
                    }
                    .disabled(busy || plan.setupKind == "windows-woa")
                }
            }
            if busy {
                ProgressView("Running x86 setup…")
            }
            if let message {
                Text(message).font(.caption).foregroundStyle(.secondary)
            }
        }
        .onAppear(perform: reload)
    }

    private func setupTitle(_ plan: X86Plan) -> String {
        switch plan.setupKind {
        case "fex-rootfs":
            return "Set up FEX RootFS"
        case "rosetta-guest-setup":
            return "Run guest setup (Rosetta)"
        case "qemu-user":
            return "Install qemu-user-static"
        case "windows-woa":
            return "Windows x64 is in-guest"
        default:
            return "Set up x86"
        }
    }

    private func persist(enabled: Bool) {
        do {
            let text = try String(contentsOf: configURL, encoding: .utf8)
            let next = VMConfigParser.replacingRunX86(in: text, enabled: enabled)
            try next.write(to: configURL, atomically: true, encoding: .utf8)
            reload()
        } catch {
            message = error.localizedDescription
        }
    }

    private func reload() {
        do {
            let text = try String(contentsOf: configURL, encoding: .utf8)
            enabled = VMConfigParser.parseRunX86(in: text)
            let helper = try VMProcessManager.helperBinaryURL()
            plan = try X86PlanRunner.plan(configURL: configURL, helperURL: helper)
            message = nil
        } catch {
            message = error.localizedDescription
        }
    }

    private func runSetup() {
        busy = true
        message = nil
        DispatchQueue.global(qos: .userInitiated).async {
            do {
                let helper = try VMProcessManager.helperBinaryURL()
                try X86PlanRunner.setup(configURL: configURL, helperURL: helper)
                DispatchQueue.main.async {
                    busy = false
                    message = "Setup finished. See ~/Library/Logs/Ternvale if a guest step was skipped."
                    reload()
                }
            } catch {
                DispatchQueue.main.async {
                    busy = false
                    message = error.localizedDescription
                }
            }
        }
    }
}
