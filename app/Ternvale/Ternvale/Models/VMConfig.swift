import Foundation

/// Subset of fields read from a library `config.toml`.
struct VMConfig: Identifiable, Equatable, Hashable {
    var id: String { name }
    let name: String
    let cpus: Int
    let ramMiB: Int
    let serialLogPath: String?
    let configURL: URL
}

enum VMConfigParser {
    /// Minimal TOML scan for the fields the UI needs (full validation is done by `ternvale run`).
    static func parse(url: URL) throws -> VMConfig {
        let text = try String(contentsOf: url, encoding: .utf8)
        let name = try requiredString(in: text, key: "name")
        let cpus = int(in: text, key: "cpus") ?? 1
        let ram = int(in: text, key: "ram_mib") ?? 256
        let serial = optionalString(in: text, key: "serial_log")
        return VMConfig(
            name: name,
            cpus: cpus,
            ramMiB: ram,
            serialLogPath: serial,
            configURL: url
        )
    }

    private static func requiredString(in text: String, key: String) throws -> String {
        guard let value = optionalString(in: text, key: key), !value.isEmpty else {
            throw ParseError.missing(key)
        }
        return value
    }

    private static func optionalString(in text: String, key: String) -> String? {
        let pattern = #"^\s*"# + key + #"\s*=\s*"([^"]*)""#
        guard let regex = try? NSRegularExpression(pattern: pattern, options: [.anchorsMatchLines]) else {
            return nil
        }
        let range = NSRange(text.startIndex..<text.endIndex, in: text)
        guard let match = regex.firstMatch(in: text, range: range),
              let capture = Range(match.range(at: 1), in: text) else {
            return nil
        }
        return String(text[capture])
    }

    private static func int(in text: String, key: String) -> Int? {
        let pattern = #"^\s*"# + key + #"\s*=\s*(\d+)\s*$"#
        guard let regex = try? NSRegularExpression(pattern: pattern, options: [.anchorsMatchLines]) else {
            return nil
        }
        let range = NSRange(text.startIndex..<text.endIndex, in: text)
        guard let match = regex.firstMatch(in: text, range: range),
              let capture = Range(match.range(at: 1), in: text) else {
            return nil
        }
        return Int(text[capture])
    }

    enum ParseError: LocalizedError {
        case missing(String)

        var errorDescription: String? {
            switch self {
            case .missing(let key):
                return "config.toml is missing \(key)"
            }
        }
    }
}
