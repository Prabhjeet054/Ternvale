import Foundation
import os

@MainActor
final class LibraryViewModel: ObservableObject {
    @Published var vms: [VMConfig] = []
    @Published var selection: VMConfig?
    @Published var phase: VMRuntimePhase = .stopped
    @Published var crashSummary: String?
    @Published var statusMessage: String?
    @Published var isBusy = false
    @Published var showCreateVM = false

    private let scanner = VMLibraryScanner()
    private let control = ControlSocketClient()
    private let processes = VMProcessManager()
    let display = VMDisplayController()
    private let log = Logger(subsystem: "com.ternvale.app", category: "library")
    private var pollTask: Task<Void, Never>?
    private var lastHelperExit: Int32?

    var liveState: String { phase.label }

    var selectedIsRunning: Bool {
        switch phase {
        case .running, .paused, .starting, .stopping:
            return true
        case .stopped, .crashed:
            return false
        }
    }

    init() {
        processes.onTerminate = { [weak self] term in
            Task { @MainActor in
                self?.handleHelperTermination(term)
            }
        }
    }

    func refreshLibrary() {
        vms = scanner.loadVMs()
        if let sel = selection, !vms.contains(where: { $0.id == sel.id }) {
            selection = vms.first
        } else if selection == nil {
            selection = vms.first
        }
        refreshStatus()
    }

    func onAppear() {
        refreshLibrary()
        pollTask = Task { [weak self] in
            while !Task.isCancelled {
                let interval: UInt64 = {
                    guard let self else { return 2_000_000_000 }
                    switch self.phase {
                    case .starting, .stopping, .running, .paused:
                        return 500_000_000
                    case .stopped, .crashed:
                        return 2_000_000_000
                    }
                }()
                try? await Task.sleep(nanoseconds: interval)
                await self?.refreshStatus()
            }
        }
    }

    func onDisappear() {
        pollTask?.cancel()
        pollTask = nil
    }

    func createVM(name: String, kernelPath: String?) {
        isBusy = true
        statusMessage = nil
        Task {
            do {
                let config = try VMCreator.create(name: name, kernelPath: kernelPath)
                refreshLibrary()
                selection = config
                phase = .stopped
                crashSummary = nil
                statusMessage = "Created \(config.name)"
                log.info("created vm \(config.name, privacy: .public)")
            } catch {
                statusMessage = error.localizedDescription
            }
            isBusy = false
        }
    }

    func refreshStatus() {
        guard let vm = selection else {
            phase = .stopped
            return
        }
        do {
            let response = try control.status(vmName: vm.name)
            if response.ok, let status = response.status {
                applySocketState(status)
                return
            }
        } catch ControlSocketClient.ReachError.staleSocket {
            if phase == .starting || phase == .running || phase == .paused {
                markCrashed(vmName: vm.name, reason: "Control socket went stale (helper may have crashed)")
            } else if phase != .stopping {
                phase = .stopped
            }
            return
        } catch ControlSocketClient.ReachError.notRunning {
            if phase == .starting, processes.isManaged(vmName: vm.name) {
                phase = .starting
            } else if phase == .stopping {
                phase = .stopped
                crashSummary = nil
            } else if phase == .running || phase == .paused {
                markCrashed(vmName: vm.name, reason: "VM is no longer reachable")
            } else if phase != .crashed {
                phase = .stopped
            }
            return
        } catch {
            log.debug("status poll failed: \(error.localizedDescription, privacy: .public)")
        }
        if control.isReachable(vmName: vm.name) {
            if phase == .starting { phase = .running }
        }
    }

    private func applySocketState(_ status: VMStatusPayload) {
        let lower = status.state.lowercased()
        if lower == "failed" || status.failure != nil {
            phase = .crashed
            crashSummary = status.failure ?? "VM entered failed state"
            return
        }
        switch lower {
        case "running":
            phase = .running
            crashSummary = nil
        case "paused":
            phase = .paused
        case "stopping":
            phase = .stopping
        case "stopped":
            if phase != .crashed { phase = .stopped }
        default:
            if phase == .starting { phase = .running }
        }
    }

    func startSelected() {
        guard let vm = selection else { return }
        isBusy = true
        statusMessage = nil
        crashSummary = nil
        phase = .starting
        lastHelperExit = nil
        display.connect(vmName: vm.name)
        Task {
            do {
                try processes.start(vm: vm)
                statusMessage = "Starting \(vm.name)…"
                log.info("started vm \(vm.name, privacy: .public)")
                try await waitForRunning(vmName: vm.name, seconds: 30)
                refreshStatus()
            } catch {
                phase = .crashed
                crashSummary = error.localizedDescription
                statusMessage = error.localizedDescription
                log.error("start failed: \(error.localizedDescription, privacy: .public)")
            }
            isBusy = false
        }
    }

    func stopSelected() {
        guard let vm = selection else { return }
        isBusy = true
        statusMessage = nil
        processes.markUserStop(vmName: vm.name)
        display.disconnect()
        phase = .stopping
        Task {
            do {
                let response = try control.shutdown(vmName: vm.name)
                if !response.ok {
                    statusMessage = response.error ?? "shutdown refused"
                } else {
                    statusMessage = "Stopping \(vm.name)…"
                }
                try await waitForSocketGone(vmName: vm.name, seconds: 30)
                phase = .stopped
                crashSummary = nil
                refreshStatus()
            } catch {
                statusMessage = error.localizedDescription
            }
            isBusy = false
        }
    }

    func pauseSelected() {
        performControl { name in try self.control.pause(vmName: name) }
    }

    func resumeSelected() {
        performControl { name in try self.control.resume(vmName: name) }
    }

    private func performControl(_ body: @escaping (String) throws -> ControlResponse) {
        guard let vm = selection else { return }
        isBusy = true
        statusMessage = nil
        let vmName = vm.name
        Task {
            do {
                let response = try body(vmName)
                if response.ok {
                    statusMessage = "OK"
                    refreshStatus()
                } else {
                    statusMessage = response.error ?? "request failed"
                }
            } catch {
                statusMessage = error.localizedDescription
            }
            isBusy = false
        }
    }

    private func handleHelperTermination(_ term: VMProcessManager.Termination) {
        guard selection?.name == term.vmName else { return }
        lastHelperExit = term.status
        if term.userRequested {
            if phase == .stopping || phase == .running || phase == .paused {
                phase = .stopped
            }
            return
        }
        markCrashed(vmName: term.vmName, reason: nil)
    }

    private func markCrashed(vmName: String, reason: String?) {
        display.disconnect()
        phase = .crashed
        let bundle = CrashBundleReader.loadSummary(vmName: vmName, helperExitCode: lastHelperExit)
        if let reason {
            crashSummary = reason + "\n" + bundle.headline
        } else {
            crashSummary = bundle.headline
            if !bundle.detail.isEmpty {
                crashSummary? += "\n" + bundle.detail
            }
        }
        log.error("vm \(vmName, privacy: .public) crashed: \(self.crashSummary ?? "", privacy: .public)")
    }

    private func waitForRunning(vmName: String, seconds: UInt64) async throws {
        let deadline = Date().addingTimeInterval(TimeInterval(seconds))
        while Date() < deadline {
            refreshStatus()
            if phase == .running { return }
            if phase == .crashed { return }
            try await Task.sleep(nanoseconds: 400_000_000)
        }
        throw TimeoutError()
    }

    private func waitForSocketGone(vmName: String, seconds: UInt64) async throws {
        let deadline = Date().addingTimeInterval(TimeInterval(seconds))
        while Date() < deadline {
            if !control.isReachable(vmName: vmName) { return }
            try await Task.sleep(nanoseconds: 100_000_000)
        }
        throw TimeoutError()
    }

    private struct TimeoutError: LocalizedError {
        var errorDescription: String? { "Timed out waiting for the VM" }
    }
}
