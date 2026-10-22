import Foundation

enum TernvalePaths {
    static let applicationSupport = FileManager.default.homeDirectoryForCurrentUser
        .appendingPathComponent("Library/Application Support/Ternvale", isDirectory: true)

    static var vmLibraryRoot: URL {
        applicationSupport.appendingPathComponent("vms", isDirectory: true)
    }

    static var runDirectory: URL {
        applicationSupport.appendingPathComponent("run", isDirectory: true)
    }

    static var hostLogDirectory: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Library/Logs/Ternvale", isDirectory: true)
    }

    static func controlSocket(for vmName: String) -> URL {
        runDirectory.appendingPathComponent("\(vmName).sock")
    }

    /// Newest `ternvale-<vm>-<YYYYMMDD-HHMMSS>.log` in the host log directory.
    static func newestHostLog(for vmName: String) -> URL? {
        guard let entries = try? FileManager.default.contentsOfDirectory(
            at: hostLogDirectory,
            includingPropertiesForKeys: [.contentModificationDateKey],
            options: [.skipsHiddenFiles]
        ) else {
            return nil
        }
        let prefix = "ternvale-\(vmName)-"
        let candidates = entries.filter { url in
            let name = url.lastPathComponent
            return name.hasPrefix(prefix) && name.hasSuffix(".log") && stampValid(name: name, vm: vmName)
        }
        return candidates.sorted { lhs, rhs in
            let l = lhs.lastPathComponent
            let r = rhs.lastPathComponent
            return l < r
        }.last
    }

    private static func stampValid(name: String, vm: String) -> Bool {
        let prefix = "ternvale-\(vm)-"
        guard name.hasPrefix(prefix), name.hasSuffix(".log") else { return false }
        let stamp = name.dropFirst(prefix.count).dropLast(4)
        let parts = stamp.split(separator: "-", maxSplits: 1)
        guard parts.count == 2, parts[0].count == 8, parts[1].count == 6 else { return false }
        return parts[0].allSatisfy(\.isNumber) && parts[1].allSatisfy(\.isNumber)
    }
}
