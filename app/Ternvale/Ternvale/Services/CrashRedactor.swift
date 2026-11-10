import Foundation

/// Strips host identity and drops guest serial / guest file contents from crash payloads.
enum CrashRedactor {
    /// File names never sent (guest serial, disks, shares).
    static let excludedNameFragments = [
        "serial",
        "guest-serial",
        ".qcow2",
        ".vhdx",
        ".vmdk",
        "disk0.",
        "rootfs",
        "clipboard",
    ]

    static func shouldExclude(filename: String) -> Bool {
        let lower = filename.lowercased()
        return excludedNameFragments.contains { lower.contains($0) }
    }

    static func redact(
        _ text: String,
        home: String = NSHomeDirectory(),
        user: String = NSUserName(),
        host: String = Host.current().localizedName ?? ""
    ) -> String {
        var out = text
        if !home.isEmpty {
            out = out.replacingOccurrences(of: home, with: "~")
        }
        let nsHome = (home as NSString).expandingTildeInPath
        if nsHome != home, !nsHome.isEmpty {
            out = out.replacingOccurrences(of: nsHome, with: "~")
        }
        if !user.isEmpty {
            out = out.replacingOccurrences(of: "/Users/\(user)", with: "/Users/USER")
            out = out.replacingOccurrences(of: user, with: "USER")
        }
        if !host.isEmpty {
            out = out.replacingOccurrences(of: host, with: "HOST")
        }
        if let name = Host.current().name, !name.isEmpty {
            out = out.replacingOccurrences(of: name, with: "HOST")
        }
        return out
    }
}
