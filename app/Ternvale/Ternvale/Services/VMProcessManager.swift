import Foundation
import os

/// Launches the bundled `ternvale` helper (`run <config>`) once per VM.
final class VMProcessManager {
    struct Termination {
        let vmName: String
        let status: Int32
        let userRequested: Bool
    }

    var onTerminate: ((Termination) -> Void)?

    private let log = Logger(subsystem: "com.ternvale.app", category: "process")
    private var processes: [String: Process] = [:]
    private var userRequestedStop: Set<String> = []

    func helperURL() throws -> URL {
        if let override = ProcessInfo.processInfo.environment["TERNVALE_HELPER_PATH"],
           FileManager.default.isExecutableFile(atPath: override) {
            return URL(fileURLWithPath: override)
        }
        let helpers = Bundle.main.bundleURL
            .appendingPathComponent("Contents/Helpers/ternvale")
        if FileManager.default.isExecutableFile(atPath: helpers.path) {
            return helpers
        }
        let dev = Bundle.main.bundleURL
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("target/debug/ternvale")
        if FileManager.default.isExecutableFile(atPath: dev.path) {
            return dev
        }
        throw HelperError.notFound
    }

    func isManaged(vmName: String) -> Bool {
        processes[vmName] != nil
    }

    func markUserStop(vmName: String) {
        userRequestedStop.insert(vmName)
    }

    func clearUserStop(vmName: String) {
        userRequestedStop.remove(vmName)
    }

    func start(vm: VMConfig) throws {
        if processes[vm.name] != nil {
            log.info("vm \(vm.name, privacy: .public) already has a helper process")
            return
        }
        userRequestedStop.remove(vm.name)
        let binary = try helperURL()
        let process = Process()
        process.executableURL = binary
        process.arguments = ["run", vm.configURL.path]
        process.currentDirectoryURL = vm.configURL.deletingLastPathComponent()
        var env = ProcessInfo.processInfo.environment
        env.removeValue(forKey: "TERNVALE_HELPER_PATH")
        process.environment = env
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        let vmName = vm.name
        process.terminationHandler = { [weak self] proc in
            guard let self else { return }
            let requested = self.userRequestedStop.contains(vmName)
            self.userRequestedStop.remove(vmName)
            self.processes.removeValue(forKey: vmName)
            self.log.info(
                "helper for \(vmName, privacy: .public) exited status=\(proc.terminationStatus) userStop=\(requested)"
            )
            let event = Termination(vmName: vmName, status: proc.terminationStatus, userRequested: requested)
            self.onTerminate?(event)
        }
        log.info("starting \(binary.path, privacy: .public) run \(vm.configURL.path, privacy: .public)")
        try process.run()
        processes[vm.name] = process
    }

    func stopAll() {
        for (name, process) in processes {
            if process.isRunning {
                log.info("terminating helper for \(name, privacy: .public)")
                process.terminate()
            }
        }
        processes.removeAll()
        userRequestedStop.removeAll()
    }

    enum HelperError: LocalizedError {
        case notFound

        var errorDescription: String? {
            "Could not find the ternvale helper (build with cargo build -p ternvale-cli or run the Xcode copy script)"
        }
    }
}
