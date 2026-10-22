import SwiftUI

struct ContentView: View {
    @StateObject private var library = LibraryViewModel()
    @StateObject private var logTailer = HostLogTailer()

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
                    onResume: library.resumeSelected
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
        .onAppear {
            library.onAppear()
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
