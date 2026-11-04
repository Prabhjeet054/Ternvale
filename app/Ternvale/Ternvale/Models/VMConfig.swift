import Foundation

/// One `[[nics]]` table from `config.toml`.
struct NicConfig: Equatable, Hashable, Identifiable {
    var id: Int
    var backend: String
    var mode: String
    var network: String
    var bridgeInterface: String

    static func loopback(id: Int) -> NicConfig {
        NicConfig(id: id, backend: "loopback", mode: "shared", network: "", bridgeInterface: "")
    }

    var isVmnet: Bool { backend == "vmnet" }
}

/// Subset of fields read from a library `config.toml`.
struct VMConfig: Identifiable, Equatable, Hashable {
    var id: String { name }
    let name: String
    let cpus: Int
    let ramMiB: Int
    let serialLogPath: String?
    let configURL: URL
    let nics: [NicConfig]
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
            configURL: url,
            nics: parseNics(in: text)
        )
    }

    static func parseNics(in text: String) -> [NicConfig] {
        var nics: [NicConfig] = []
        let parts = text.components(separatedBy: "[[nics]]")
        for (index, part) in parts.enumerated() {
            if index == 0 { continue }
            let block = part.components(separatedBy: "[[").first ?? part
            let backend = tableString(in: block, key: "backend") ?? "loopback"
            nics.append(
                NicConfig(
                    id: index - 1,
                    backend: backend,
                    mode: tableString(in: block, key: "mode") ?? "shared",
                    network: tableString(in: block, key: "network") ?? "",
                    bridgeInterface: tableString(in: block, key: "bridge_interface") ?? ""
                )
            )
        }
        return nics
    }

    /// Replace every `[[nics]]` table; leave the rest of the document intact.
    static func replacingNics(in text: String, nics: [NicConfig]) -> String {
        let stripped = stripNicTables(from: text).trimmingCharacters(in: .newlines)
        var out = stripped + "\n"
        if nics.isEmpty { return out }
        out += "\n"
        for nic in nics {
            out += "[[nics]]\n"
            out += "backend = \"\(nic.backend)\"\n"
            if nic.backend == "vmnet" {
                out += "mode = \"\(nic.mode)\"\n"
                if nic.mode == "host-only", !nic.network.isEmpty {
                    out += "network = \"\(nic.network)\"\n"
                }
                if nic.mode == "bridged", !nic.bridgeInterface.isEmpty {
                    out += "bridge_interface = \"\(nic.bridgeInterface)\"\n"
                }
            }
            out += "\n"
        }
        return out
    }

    private static func stripNicTables(from text: String) -> String {
        var out: [String] = []
        var skipping = false
        for line in text.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline) {
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed == "[[nics]]" {
                skipping = true
                continue
            }
            if skipping {
                if trimmed.hasPrefix("[") {
                    skipping = false
                    out.append(String(line))
                }
                continue
            }
            out.append(String(line))
        }
        return out.joined(separator: "\n")
    }

    private static func tableString(in text: String, key: String) -> String? {
        optionalString(in: text, key: key)
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
