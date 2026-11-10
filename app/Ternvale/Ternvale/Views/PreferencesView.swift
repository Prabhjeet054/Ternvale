import SwiftUI

/// App-wide update channel, install consent, and crash-report opt-in.
struct PreferencesView: View {
    @AppStorage(AppSettings.updateChannelKey) private var channel = "stable"
    @AppStorage(AppSettings.autoInstallUpdatesKey) private var autoInstall = false
    @AppStorage(AppSettings.crashReportingOptInKey) private var crashOptIn = false
    var onCheckUpdates: () -> Void = {
        NotificationCenter.default.post(name: .ternvaleCheckUpdates, object: nil)
    }
    var onConsentChanged: () -> Void = {}

    var body: some View {
        Form {
            Section("Updates") {
                Picker("Channel", selection: $channel) {
                    Text("Stable").tag("stable")
                    Text("Beta").tag("beta")
                }
                Toggle("Download and install updates automatically", isOn: $autoInstall)
                Text("Off by default. Sparkle still verifies EdDSA signatures and macOS still requires a notarized Developer ID zip. Automatic install only runs when this is on.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Button("Check for Updates…", action: onCheckUpdates)
            }
            Section("Crash reports") {
                Toggle("Allow sending crash reports", isOn: $crashOptIn)
                Text("Off by default. No telemetry. If a VM or the app crashes, Ternvale shows a preview of every file and field before anything is sent.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Section("Privacy") {
                Text("See docs/PRIVACY.md in the Ternvale source tree for what is and is not collected.")
                    .font(.caption)
            }
        }
        .formStyle(.grouped)
        .padding()
        .frame(minWidth: 420, minHeight: 280)
        .onChange(of: autoInstall) { _, _ in
            SparkleUpdater.syncDefaults()
            onConsentChanged()
        }
        .onChange(of: channel) { _, _ in
            SparkleUpdater.syncDefaults()
            onConsentChanged()
        }
    }
}
