import Foundation

/// JSON published at the URL we host (`docs/updates/latest.json`).
struct UpdateFeed: Codable, Equatable {
    let version: String
    let build: Int?
    let notes: String?
    /// Informational page only; never fetched as an installer.
    let url: String?
}

enum SemVer {
    /// Strict `major.minor.patch` compare; missing parts are 0.
    static func isNewer(_ remote: String, than current: String) -> Bool {
        compare(parse(remote), parse(current)) == .orderedDescending
    }

    static func parse(_ text: String) -> (UInt32, UInt32, UInt32) {
        let core = text.split(whereSeparator: { $0 == "-" || $0 == "+" }).first.map(String.init) ?? text
        let parts = core.split(separator: ".").map(String.init)
        func num(_ i: Int) -> UInt32 {
            guard i < parts.count, let v = UInt32(parts[i]) else { return 0 }
            return v
        }
        return (num(0), num(1), num(2))
    }

    static func compare(_ lhs: (UInt32, UInt32, UInt32), _ rhs: (UInt32, UInt32, UInt32)) -> ComparisonResult {
        if lhs.0 != rhs.0 { return lhs.0 > rhs.0 ? .orderedDescending : .orderedAscending }
        if lhs.1 != rhs.1 { return lhs.1 > rhs.1 ? .orderedDescending : .orderedAscending }
        if lhs.2 != rhs.2 { return lhs.2 > rhs.2 ? .orderedDescending : .orderedAscending }
        return .orderedSame
    }
}

enum UpdateFeedURL {
    static let defaultString = "https://updates.ternvale.dev/latest.json"

    static func resolved() -> URL? {
        if let env = ProcessInfo.processInfo.environment["TERNVALE_UPDATE_FEED_URL"], !env.isEmpty {
            return URL(string: env)
        }
        if let plist = Bundle.main.object(forInfoDictionaryKey: "TERNVALEUpdateFeedURL") as? String,
           !plist.isEmpty
        {
            return URL(string: plist)
        }
        return URL(string: defaultString)
    }
}
