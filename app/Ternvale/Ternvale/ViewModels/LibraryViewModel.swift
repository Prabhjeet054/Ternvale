import Combine
import Foundation
import os

@MainActor
final class LibraryViewModel: ObservableObject {
    @Published var vms: [VMConfig] = []
    @Published var selection: VMConfig?
    @Published var phase: VMRuntimePhase = .stopped
    @Published var crashSummary: String?
    @Published var statusMessage: String?
    @Published var toolsOutdatedMessage: String?
    @Published var isBusy = false
    @Published var showCreateVM = false
    @Published var showVMSettings = false
    @Published var showPreferences = false
    @Published var crashOffer: CrashReportPayload?
    @Published var showCrashOffer = false
    /// From `status.capabilities.custom_display_pipeline`. Missing: assume Metal.
    @Published var usesCustomDisplayPipeline = true

    private let scanner = VMLibraryScanner()
    private let control = ControlSocketClient()
    private let processes = VMProcessManager()
    let display = VMDisplayController()
    let seamless: SeamlessWindowsCoordinator
    private let log = Logger(subsystem: "com.ternvale.app", category: "library")
    private var pollTask: Task<Void, Never>?
    private var lastHelperExit: Int32?
    private var seamlessSink: AnyCancellable?

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
        seamless = SeamlessWindowsCoordinator(display: display)
        seamlessSink = seamless.objectWillChange.sink { [weak self] _ in
            self?.objectWillChange.send()
        }
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
        if CrashHeartbeat.takeLaunchOffer() {
            presentCrashOffer(vmName: "app", reason: "The previous Ternvale app session ended without a clean shutdown.")
        }
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

    func createVM(_ request: NewVMRequest) {
        isBusy = true
        statusMessage = nil
        Task {
            do {
                let config: VMConfig
                if request.windowsUnattended {
                    let helper = try processes.helperURL()
                    config = try VMCreator.createWindows(
                        name: request.name,
                        iso: request.windowsIso,
                        password: request.password,
                        productKey: request.productKey.isEmpty ? nil : request.productKey,
                        helper: helper
                    )
                } else {
                    config = try VMCreator.create(name: request.name, kernelPath: request.kernelPath)
                }
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
                if phase == .starting || phase == .running {
                    if let stats = try? control.queryStats(vmName: vm.name),
                       let bytes = stats.stats?.metrics?.blk.bytes
                    {
                        let agent = status.agent?.state ?? "waiting"
                        if status.agent?.state == "connected" {
                            statusMessage = "Guest agent connected (disk I/O \(bytes) bytes)"
                        } else {
                            statusMessage = "Installing… disk I/O \(bytes) bytes, agent \(agent)"
                        }
                    }
                }
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
                toolsOutdatedMessage = nil
            } else if phase == .running || phase == .paused {
                markCrashed(vmName: vm.name, reason: "VM is no longer reachable")
            } else if phase != .crashed {
                phase = .stopped
                toolsOutdatedMessage = nil
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
        if let custom = status.capabilities?.customDisplayPipeline {
            usesCustomDisplayPipeline = custom
        }
        if !usesCustomDisplayPipeline {
            display.disconnect()
        }
        if status.toolsOutdated == true {
            let iso = status.toolsIsoAgent ?? "newer"
            toolsOutdatedMessage =
                "Guest tools are out of date (ISO agent \(iso)). Reinstall from ternvale-tools.iso."
            log.info("guest tools outdated iso_agent=\(iso, privacy: .public)")
        } else {
            toolsOutdatedMessage = nil
        }
    }

    func startSelected() {
        guard let vm = selection else { return }
        isBusy = true
        statusMessage = nil
        crashSummary = nil
        phase = .starting
        lastHelperExit = nil
        if usesCustomDisplayPipeline {
            display.connect(vmName: vm.name)
        }
        Task {
            do {
                try processes.start(
                    vm: vm,
                    seamlessWindows: AppSettings.experimentalSeamlessWindows
                )
                if AppSettings.experimentalSeamlessWindows {
                    seamless.start(vmName: vm.name)
                }
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

    func sendFeedback() {
        guard let vm = selection else {
            statusMessage = FeedbackMailer.FeedbackError.noVm.localizedDescription
            return
        }
        isBusy = true
        statusMessage = "Preparing crash bundle…"
        let name = vm.name
        Task {
            do {
                let stamp = ISO8601DateFormatter().string(from: Date()).replacingOccurrences(of: ":", with: "")
                let zip = FileManager.default.temporaryDirectory
                    .appendingPathComponent("ternvale-report-\(name)-\(stamp).zip")
                let helper = try? processes.helperURL()
                try FeedbackMailer.makeZip(
                    vmName: name,
                    helper: helper,
                    config: vm.configURL,
                    out: zip
                )
                try FeedbackMailer.presentDraft(vmName: name, zip: zip)
                statusMessage = "Mail draft opened — send only if you want to share the zip"
                log.info("feedback draft opened for \(name, privacy: .public)")
            } catch {
                statusMessage = error.localizedDescription
                log.error("feedback failed: \(error.localizedDescription, privacy: .public)")
            }
            isBusy = false
        }
    }

    func disableSeamlessWhileRunning() {
        guard seamless.isActive else { return }
        seamless.stop()
        log.info("seamless disabled from settings; Step 56 embedded display restored for running VM")
        statusMessage = "Seamless off — embedded display restored (restart VM to clear host env)"
    }

    func stopSelected() {
        guard let vm = selection else { return }
        isBusy = true
        statusMessage = nil
        processes.markUserStop(vmName: vm.name)
        seamless.stop()
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
        let bridged = VmnetHelperService.messageIfBridgedDenied(term.stderr)
        markCrashed(vmName: term.vmName, reason: bridged)
    }

    private func markCrashed(vmName: String, reason: String?) {
        seamless.stop()
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
        presentCrashOffer(vmName: vmName, reason: crashSummary)
    }

    func presentCrashOffer(vmName: String, reason: String?) {
        let files = CrashBundleReader.previewFiles(vmName: vmName)
        let version = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String ?? "0.0.0"
        crashOffer = CrashReportPayload.preview(
            files: files,
            fields: [
                ("vm_name", vmName),
                ("app_version", version),
                ("reason", reason ?? ""),
            ],
            appVersion: version
        )
        showCrashOffer = true
        log.info("crash preview offered vm=\(vmName, privacy: .public) files=\(files.count, privacy: .public)")
    }

    func sendCrashOffer() {
        guard let payload = crashOffer else { return }
        isBusy = true
        Task {
            do {
                try await CrashReporter.send(payload)
                statusMessage = "Crash report sent"
                showCrashOffer = false
                crashOffer = nil
            } catch {
                statusMessage = error.localizedDescription
                log.error("crash send failed: \(error.localizedDescription, privacy: .public)")
            }
            isBusy = false
        }
    }

    func dismissCrashOffer() {
        showCrashOffer = false
        crashOffer = nil
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
