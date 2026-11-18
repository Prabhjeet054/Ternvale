import Foundation
import os

/// `ternvale x86 plan --json` (capability flags only; no backend name).
struct X86Plan: Equatable, Decodable {
    let ok: Bool
    let vm: String
    let profile: String
    let requested: String
    let engine: String
    let enabled: Bool
    let available: Bool
    let unavailableReason: String?
    let setupKind: String
    let setupHint: String
    let needsRunningVm: Bool
    let caps: Caps

    struct Caps: Equatable, Decodable {
        let rosetta: Bool
        let fex: Bool
        let qemuUser: Bool

        enum CodingKeys: String, CodingKey {
            case rosetta, fex
            case qemuUser = "qemu_user"
        }
    }

    enum CodingKeys: String, CodingKey {
        case ok, vm, profile, requested, engine, enabled, available, caps
        case unavailableReason = "unavailable_reason"
        case setupKind = "setup_kind"
        case setupHint = "setup_hint"
        case needsRunningVm = "needs_running_vm"
    }

    var engineSummary: String {
        if profile == "windows" {
            return "Windows on ARM x64 (in-guest; Ternvale does not control it)"
        }
        if engine == "none" {
            return "No usermode x86 engine"
        }
        return "Engine: \(engine)"
    }
}

enum X86PlanRunner {
    private static let log = Logger(subsystem: "com.ternvale.app", category: "x86")

    static func plan(configURL: URL, helperURL: URL) throws -> X86Plan {
        let data = try run(
            helperURL: helperURL,
            arguments: ["--json", "x86", "plan", "--config", configURL.path],
            timeout: 20
        )
        return try JSONDecoder().decode(X86Plan.self, from: data)
    }

    /// Guest package/network failures must not spin forever (apt/dnf ~10s timeout).
    static let setupTimeout: TimeInterval = 90

    static func setup(configURL: URL, helperURL: URL) throws {
        _ = try run(
            helperURL: helperURL,
            arguments: ["--json", "x86", "setup", "--config", configURL.path, "--yes"],
            timeout: setupTimeout
        )
    }

    private static func run(helperURL: URL, arguments: [String], timeout: TimeInterval) throws -> Data {
        let process = Process()
        process.executableURL = helperURL
        process.arguments = arguments
        let out = Pipe()
        let err = Pipe()
        process.standardOutput = out
        process.standardError = err
        log.info("x86 helper \(arguments.joined(separator: " "), privacy: .public)")
        try process.run()
        let deadline = Date().addingTimeInterval(timeout)
        while process.isRunning, Date() < deadline {
            Thread.sleep(forTimeInterval: 0.05)
        }
        if process.isRunning {
            process.terminate()
            throw X86HelperError.timeout(logHint: TernvalePaths.hostLogDirectory.path)
        }
        let stdout = out.fileHandleForReading.readDataToEndOfFile()
        let stderr = String(data: err.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
        if process.terminationStatus != 0 {
            throw X86HelperError.failed(
                status: process.terminationStatus,
                stderr: stderr,
                logHint: TernvalePaths.hostLogDirectory.path
            )
        }
        return stdout
    }
}

enum X86HelperError: LocalizedError {
    case timeout(logHint: String)
    case failed(status: Int32, stderr: String, logHint: String)

    var errorDescription: String? {
        switch self {
        case .timeout(let logHint):
            return "x86 setup timed out. Host logs: \(logHint)"
        case .failed(let status, let stderr, let logHint):
            let detail = Self.detail(fromStderr: stderr)
            if detail.isEmpty {
                return "x86 helper exited \(status). Host logs: \(logHint)"
            }
            if detail.contains("logs:") || detail.contains("Library/Logs/Ternvale") {
                return detail
            }
            return "\(detail) (logs: \(logHint))"
        }
    }

    /// Last `error:` line from the helper, or a compact fallback.
    static func detail(fromStderr stderr: String) -> String {
        let lines = stderr.split(whereSeparator: \.isNewline).map {
            $0.trimmingCharacters(in: .whitespaces)
        }
        if let last = lines.last(where: { $0.lowercased().hasPrefix("error:") }) {
            return String(last)
        }
        let compact = stderr.trimmingCharacters(in: .whitespacesAndNewlines)
        if compact.count <= 280 {
            return compact
        }
        return String(compact.suffix(280))
    }
}
