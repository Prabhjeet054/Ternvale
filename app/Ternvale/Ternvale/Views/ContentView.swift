import SwiftUI

struct ContentView: View {
    @StateObject private var library = LibraryViewModel()
    @StateObject private var logTailer = HostLogTailer()
    @StateObject private var updates = UpdateChecker()
    @StateObject private var sparkle = SparkleUpdater()

    var body: some View {
        NavigationSplitView {
            VMListView(
                vms: library.vms,
                selection: $library.selection,
                liveState: library.liveState,
                onRefresh: library.refreshLibrary,
                onCreate: { library.showCreateVM = true }
            )
            .navigationSplitViewColumnWidth(min: 220, ideal: 260)
        } detail: {
            VStack(spacing: 0) {
                if let banner = updates.banner {
                    UpdateBannerView(banner: banner, onDismiss: updates.dismiss)
                    Divider()
                }
                VMControlBar(
                    vm: library.selection,
                    state: library.liveState,
                    phase: library.phase,
                    crashSummary: library.crashSummary,
                    isRunning: library.selectedIsRunning,
                    isBusy: library.isBusy,
                    statusMessage: library.statusMessage,
                    toolsOutdatedMessage: library.toolsOutdatedMessage,
                    onStart: library.startSelected,
                    onStop: library.stopSelected,
                    onPause: library.pauseSelected,
                    onResume: library.resumeSelected,
                    onSeamlessDisabled: library.disableSeamlessWhileRunning,
                    onSendFeedback: library.sendFeedback,
                    onSettings: { library.showVMSettings = true }
                )
                Divider()
                VMDisplayPanel(
                    controller: library.display,
                    seamless: library.seamless,
                    isActive: library.selectedIsRunning,
                    usesCustomDisplayPipeline: library.usesCustomDisplayPipeline
                )
                Divider()
                LogPanelView(text: logTailer.text, path: logTailer.path)
            }
        }
        .sheet(isPresented: $library.showVMSettings) {
            if let vm = library.selection {
                NavigationStack {
                    VMSettingsView(configURL: vm.configURL, initial: vm.nics) { _ in
                        library.refreshLibrary()
                    }
                }
            }
        }
        .sheet(isPresented: $library.showCreateVM) {
            NavigationStack {
                CreateVMView { request in
                    library.createVM(request)
                }
            }
        }
        .sheet(isPresented: $library.showCrashOffer) {
            if let payload = library.crashOffer {
                CrashReportPreviewView(
                    payload: payload,
                    optedIn: AppSettings.crashReportingOptIn,
                    onSend: library.sendCrashOffer,
                    onCancel: library.dismissCrashOffer
                )
            }
        }
        .sheet(isPresented: $library.showPreferences) {
            PreferencesView(
                onCheckUpdates: {
                    sparkle.checkForUpdates()
                    updates.check()
                },
                onConsentChanged: sparkle.applyConsent
            )
        }
        .onAppear {
            library.onAppear()
            sparkle.start()
            updates.check()
        }
        .onDisappear {
            library.onDisappear()
        }
        .onReceive(NotificationCenter.default.publisher(for: .ternvaleCheckUpdates)) { _ in
            sparkle.checkForUpdates()
            updates.check()
        }
        .onReceive(NotificationCenter.default.publisher(for: .ternvaleSendFeedback)) { _ in
            library.sendFeedback()
        }
        .onReceive(NotificationCenter.default.publisher(for: .ternvaleShowPreferences)) { _ in
            library.showPreferences = true
        }
        .onChange(of: library.selection?.name) { _, name in
            logTailer.follow(vmName: name)
        }
        .onChange(of: library.phase) { _, phase in
            if phase == .starting || phase == .running {
                logTailer.follow(vmName: library.selection?.name)
            }
        }
    }
}
