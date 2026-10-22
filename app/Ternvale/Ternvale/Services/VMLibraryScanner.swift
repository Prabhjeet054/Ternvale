import Foundation
import os

final class VMLibraryScanner {
    private let log = Logger(subsystem: "com.ternvale.app", category: "library")

    func loadVMs() -> [VMConfig] {
        let root = TernvalePaths.vmLibraryRoot
        guard let entries = try? FileManager.default.contentsOfDirectory(
            at: root,
            includingPropertiesForKeys: [.isDirectoryKey],
            options: [.skipsHiddenFiles]
        ) else {
            log.debug("no vm library at \(root.path, privacy: .public)")
            return []
        }
        var configs: [VMConfig] = []
        for dir in entries {
            let configURL = dir.appendingPathComponent("config.toml")
            guard (try? configURL.checkResourceIsReachable()) == true else { continue }
            do {
                let config = try VMConfigParser.parse(url: configURL)
                configs.append(config)
            } catch {
                log.warning("skip \(configURL.path, privacy: .public): \(String(describing: error))")
            }
        }
        return configs.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }
}
