import Foundation
import os

/// Creates a new library VM directory with a minimal `config.toml`.
enum VMCreator {
    private static let log = Logger(subsystem: "com.ternvale.app", category: "library")

    enum CreateError: LocalizedError {
        case invalidName
        case alreadyExists
        case io(String)

        var errorDescription: String? {
            switch self {
            case .invalidName:
                "Name must be non-empty ASCII letters, digits, '-' or '_'."
            case .alreadyExists:
                "A VM with this name already exists."
            case .io(let message):
                message
            }
        }
    }

    static func create(name: String, kernelPath: String?) throws -> VMConfig {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard isValidName(trimmed) else { throw CreateError.invalidName }
        let vmDir = TernvalePaths.vmLibraryRoot.appendingPathComponent(trimmed, isDirectory: true)
        if FileManager.default.fileExists(atPath: vmDir.path) {
            throw CreateError.alreadyExists
        }
        let disksDir = vmDir.appendingPathComponent("disks", isDirectory: true)
        do {
            try FileManager.default.createDirectory(at: disksDir, withIntermediateDirectories: true)
        } catch {
            throw CreateError.io(error.localizedDescription)
        }
        let kernel = kernelPath?.trimmingCharacters(in: .whitespacesAndNewlines).nilIfEmpty
            ?? "/path/to/Image"
        let disk = disksDir.appendingPathComponent("disk0.img")
        let serial = vmDir.appendingPathComponent("serial.log")
        let configURL = vmDir.appendingPathComponent("config.toml")
        let text = """
        name = "\(trimmed)"
        cpus = 1
        ram_mib = 256
        kernel = "\(kernel)"
        serial_log = "\(serial.path)"
        cmdline = "console=ttyAMA0"

        [[disks]]
        path = "\(disk.path)"
        read_only = false
        """
        do {
            try text.write(to: configURL, atomically: true, encoding: .utf8)
        } catch {
            throw CreateError.io(error.localizedDescription)
        }
        log.info("created vm library entry \(trimmed, privacy: .public) at \(configURL.path, privacy: .public)")
        return try VMConfigParser.parse(url: configURL)
    }

    /// `ternvale create --windows --unattended`. Password travels only in the helper env.
    static func createWindows(
        name: String,
        iso: String,
        password: String,
        productKey: String?,
        helper: URL
    ) throws -> VMConfig {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard isValidName(trimmed) else { throw CreateError.invalidName }
        try KeychainStore.store(account: KeychainStore.passwordAccount(vm: trimmed), secret: password)
        if let key = productKey?.trimmingCharacters(in: .whitespacesAndNewlines), !key.isEmpty {
            try KeychainStore.store(account: KeychainStore.productKeyAccount(vm: trimmed), secret: key)
        }
        let process = Process()
        process.executableURL = helper
        var args = [
            "create", "--windows", "--unattended",
            "--name", trimmed,
            "--iso", iso,
            "--password-env", "TERNVALE_WIN_PASSWORD",
        ]
        if let key = productKey?.trimmingCharacters(in: .whitespacesAndNewlines), !key.isEmpty {
            args += ["--product-key-env", "TERNVALE_WIN_PRODUCT_KEY"]
        }
        process.arguments = args
        var env = ProcessInfo.processInfo.environment
        env["TERNVALE_WIN_PASSWORD"] = password
        if let key = productKey?.trimmingCharacters(in: .whitespacesAndNewlines), !key.isEmpty {
            env["TERNVALE_WIN_PRODUCT_KEY"] = key
        }
        process.environment = env
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = pipe
        try process.run()
        process.waitUntilExit()
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let text = String(data: data, encoding: .utf8) ?? ""
        if process.terminationStatus != 0 {
            log.error("ternvale create failed status=\(process.terminationStatus) \(text, privacy: .public)")
            throw CreateError.io(text.isEmpty ? "ternvale create failed" : lastLines(text))
        }
        log.info("created windows vm \(trimmed, privacy: .public)")
        let configURL = TernvalePaths.vmLibraryRoot
            .appendingPathComponent(trimmed, isDirectory: true)
            .appendingPathComponent("config.toml")
        return try VMConfigParser.parse(url: configURL)
    }

    private static func lastLines(_ text: String) -> String {
        let lines = text.split(separator: "\n")
        return lines.suffix(12).joined(separator: "\n")
    }

    private static func isValidName(_ name: String) -> Bool {
        !name.isEmpty && name.allSatisfy { ch in
            ch.isASCII && (ch.isLetter || ch.isNumber || ch == "-" || ch == "_")
        }
    }
}

private extension String {
    var nilIfEmpty: String? { isEmpty ? nil : self }
}
