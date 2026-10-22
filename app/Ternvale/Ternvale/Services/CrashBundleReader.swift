import Foundation
import os

/// Reads Step 30 sidecars beside `ternvale-<vm>-<stamp>.log` (`.summary.txt`, `.run.json`).
enum CrashBundleReader {
    private static let log = Logger(subsystem: "com.ternvale.app", category: "crash")

    struct Summary {
        let headline: String
        let detail: String
        let hostLog: URL?
    }

    /// `x.log` → `x.summary.txt` (same stem as `ternvale-cli` `diag::sidecar`).
    static func sidecarPath(hostLog: URL, suffix: String) -> URL {
        let dir = hostLog.deletingLastPathComponent()
        let base = hostLog.deletingPathExtension().lastPathComponent
        return dir.appendingPathComponent(base + suffix)
    }

    /// Best-effort summary for the newest host log of `vmName`.
    static func loadSummary(vmName: String, helperExitCode: Int32?) -> Summary {
        guard let hostLog = TernvalePaths.newestHostLog(for: vmName) else {
            let code = helperExitCode.map { " (helper exit \($0))" } ?? ""
            return Summary(
                headline: "VM \(vmName) ended unexpectedly",
                detail: "No host log found yet.\(code)",
                hostLog: nil
            )
        }
        let summaryURL = sidecarPath(hostLog: hostLog, suffix: ".summary.txt")
        let runURL = sidecarPath(hostLog: hostLog, suffix: ".run.json")
        var lines: [String] = []
        if let text = try? String(contentsOf: summaryURL, encoding: .utf8) {
            let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                lines.append(trimmed.components(separatedBy: .newlines).first ?? trimmed)
            }
        }
        if let manifest = try? String(contentsOf: runURL, encoding: .utf8) {
            if let range = manifest.range(of: #""pid"\s*:\s*\d+"#, options: .regularExpression) {
                lines.append("run manifest: " + manifest[range].replacingOccurrences(of: "\"", with: ""))
            }
        }
        if let code = helperExitCode, code != 0 {
            lines.append("helper exit code: \(code)")
        }
        let mmio = sidecarPath(hostLog: hostLog, suffix: ".mmio.txt")
        if FileManager.default.fileExists(atPath: mmio.path) {
            lines.append("mmio trace: \(mmio.lastPathComponent)")
        }
        let headline = lines.first ?? "VM \(vmName) stopped unexpectedly"
        let detail = lines.dropFirst().joined(separator: "\n")
        log.info("crash summary for \(vmName, privacy: .public): \(headline, privacy: .public)")
        return Summary(headline: headline, detail: detail, hostLog: hostLog)
    }
}
