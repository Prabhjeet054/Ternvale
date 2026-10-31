import SwiftUI

struct ContentView: View {
    @StateObject private var library = LibraryViewModel()
    @StateObject private var logTailer = HostLogTailer()
    @StateObject private var updates = UpdateChecker()

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
                    onStart: library.startSelected,
                    onStop: library.stopSelected,
                    onPause: library.pauseSelected,
                    onResume: library.resumeSelected,
                    onSeamlessDisabled: library.disableSeamlessWhileRunning,
                    onSendFeedback: library.sendFeedback
                )
                Divider()
                VMDisplayPanel(
                    controller: library.display,
                    seamless: library.seamless,
                    isActive: library.selectedIsRunning
                )
                Divider()
                LogPanelView(text: logTailer.text, path: logTailer.path)
            }
        }
        .sheet(isPresented: $library.showCreateVM) {
            NavigationStack {
                CreateVMView { name, kernel in
                    library.createVM(name: name, kernelPath: kernel)
                }
            }
        }
        .commands {
            CommandGroup(replacing: .newItem) {}
            CommandGroup(after: .help) {
                Button("Send Feedback…") {
                    library.sendFeedback()
                }
                .disabled(library.selection == nil || library.isBusy)
            }
        }
        .onAppear {
            library.onAppear()
            updates.check()
        }
        .onDisappear {
            library.onDisappear()
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
