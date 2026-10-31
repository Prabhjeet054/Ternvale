import AppKit
import Foundation
import os

/// Builds a Step 30-style zip and opens a mail draft. Never uploads.
enum FeedbackMailer {
    static let log = Logger(subsystem: "com.ternvale.app", category: "feedback")

    /// Default inbox documented in `docs/BETA_CHECKLIST.md`.
    static let defaultRecipient = "beta@ternvale.dev"

    /// Sidecar suffixes written beside `ternvale-<vm>-<stamp>.log` (Step 30 / `diag`).
    static let sidecarSuffixes = [
        ".summary.txt",
        ".run.json",
        ".mmio.txt",
        ".dtb",
        ".tpm.txt",
    ]

    static func recipient() -> String {
        if let env = ProcessInfo.processInfo.environment["TERNVALE_FEEDBACK_EMAIL"], !env.isEmpty {
            return env
        }
        if let plist = Bundle.main.object(forInfoDictionaryKey: "TERNVALEFeedbackEmail") as? String,
           !plist.isEmpty
        {
            return plist
        }
        return defaultRecipient
    }

    /// Files to put in a fallback zip when `ternvale report` cannot run.
    static func collectSidecars(hostLog: URL, fileExists: (URL) -> Bool = {
        FileManager.default.fileExists(atPath: $0.path)
    }) -> [URL] {
        var files: [URL] = []
        if fileExists(hostLog) {
            files.append(hostLog)
        }
        for suffix in sidecarSuffixes {
            let side = CrashBundleReader.sidecarPath(hostLog: hostLog, suffix: suffix)
            if fileExists(side) {
                files.append(side)
            }
        }
        return files
    }

    static func mailtoURL(to: String, subject: String, body: String) -> URL? {
        var comps = URLComponents()
        comps.scheme = "mailto"
        comps.path = to
        comps.queryItems = [
            URLQueryItem(name: "subject", value: subject),
            URLQueryItem(name: "body", value: body),
        ]
        return comps.url
    }

    static func mailBody(vmName: String, zipName: String) -> String {
        """
        Ternvale beta feedback for VM "\(vmName)".
        Attach \(zipName) if it is not already attached (Step 30 crash bundle).
        Unzip it first: README.txt, config.toml, doctor.txt, and host.log (if present)
        should show the failure without extra files.

        macOS: (paste `sw_vers`)
        Chip: (paste `uname -m` — must be arm64)
        What happened:


        This mail is opened on your Mac only. Ternvale does not upload logs.
        """
    }

    /// Zip via `ternvale report` when possible; otherwise zip local sidecars with `ditto`.
    static func makeZip(vmName: String, helper: URL?, config: URL?, out: URL) throws {
        if let helper {
            let process = Process()
            process.executableURL = helper
            var args = ["report", vmName, "--out", out.path]
            if let config {
                args.append(contentsOf: ["--config", config.path])
            }
            process.arguments = args
            let err = Pipe()
            process.standardError = err
            process.standardOutput = Pipe()
            log.info("running ternvale report for \(vmName, privacy: .public)")
            try process.run()
            process.waitUntilExit()
            if process.terminationStatus == 0, FileManager.default.fileExists(atPath: out.path) {
                return
            }
            let msg = String(data: err.fileHandleForReading.readDataToEndOfFile(), encoding: .utf8) ?? ""
            log.warning(
                "ternvale report failed status=\(process.terminationStatus) \(msg, privacy: .public); falling back to local sidecars"
            )
        }
        try zipSidecars(vmName: vmName, config: config, out: out)
    }

    static func zipSidecars(vmName: String, config: URL?, out: URL) throws {
        var files: [URL] = []
        if let hostLog = TernvalePaths.newestHostLog(for: vmName) {
            files.append(contentsOf: collectSidecars(hostLog: hostLog))
        }
        if let config, FileManager.default.fileExists(atPath: config.path) {
            files.append(config)
        }
        guard !files.isEmpty else {
            throw FeedbackError.noLogs(vmName)
        }
        let staging = out.deletingLastPathComponent().appendingPathComponent(
            out.deletingPathExtension().lastPathComponent,
            isDirectory: true
        )
        let fm = FileManager.default
        if fm.fileExists(atPath: staging.path) {
            try fm.removeItem(at: staging)
        }
        try fm.createDirectory(at: staging, withIntermediateDirectories: true)
        for file in files {
            try fm.copyItem(
                at: file,
                to: staging.appendingPathComponent(file.lastPathComponent)
            )
        }
        let process = Process()
        process.executableURL = URL(fileURLWithPath: "/usr/bin/ditto")
        process.arguments = ["-c", "-k", "--keepParent", staging.path, out.path]
        try process.run()
        process.waitUntilExit()
        try? fm.removeItem(at: staging)
        guard process.terminationStatus == 0 else {
            throw FeedbackError.zipFailed
        }
        log.info("wrote fallback feedback zip \(out.path, privacy: .public)")
    }

    @MainActor
    static func presentDraft(vmName: String, zip: URL) throws {
        let subject = "Ternvale beta feedback — \(vmName)"
        let body = mailBody(vmName: vmName, zipName: zip.lastPathComponent)
        let to = recipient()
        if let service = NSSharingService(named: .composeEmail) {
            service.recipients = [to]
            service.subject = subject
            log.info("opening NSSharingService composeEmail to \(to, privacy: .public)")
            service.perform(withItems: [body, zip])
            return
        }
        log.warning("NSSharingService composeEmail unavailable; mailto + reveal zip")
        NSWorkspace.shared.activateFileViewerSelecting([zip])
        if let url = mailtoURL(to: to, subject: subject, body: body + "\n\nZip revealed in Finder: \(zip.path)") {
            NSWorkspace.shared.open(url)
        } else {
            throw FeedbackError.mailUnavailable
        }
    }

    enum FeedbackError: LocalizedError {
        case noLogs(String)
        case zipFailed
        case mailUnavailable
        case noVm

        var errorDescription: String? {
            switch self {
            case let .noLogs(name):
                "No host logs or config.toml for \(name). Create the VM, then use `ternvale report \(name)`."
            case .zipFailed:
                "Could not zip the crash bundle"
            case .mailUnavailable:
                "No mail client available. Run `ternvale report <vm>` and attach the zip to beta@ternvale.dev"
            case .noVm:
                "Select a VM first"
            }
        }
    }
}
