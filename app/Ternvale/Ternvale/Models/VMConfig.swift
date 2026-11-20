import Foundation

/// One `[[nics]]` table from `config.toml`.
struct NicConfig: Equatable, Hashable, Identifiable {
    var id: Int
    var backend: String
    var mode: String
    var network: String
    var bridgeInterface: String
    /// Lines other than backend/mode/network/bridge so Settings saves keep policy and forwards.
    var extraLines: [String]

    static func loopback(id: Int) -> NicConfig {
        NicConfig(
            id: id, backend: "loopback", mode: "shared", network: "", bridgeInterface: "",
            extraLines: [])
    }

    static func user(id: Int) -> NicConfig {
        NicConfig(
            id: id, backend: "user", mode: "shared", network: "", bridgeInterface: "", extraLines: [])
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
            let backend = tableString(in: block, key: "backend") ?? "user"
            nics.append(
                NicConfig(
                    id: index - 1,
                    backend: backend,
                    mode: tableString(in: block, key: "mode") ?? "shared",
                    network: tableString(in: block, key: "network") ?? "",
                    bridgeInterface: tableString(in: block, key: "bridge_interface") ?? "",
                    extraLines: extraNicLines(in: block)
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
            for line in nic.extraLines {
                out += line + "\n"
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

    private static let nicCoreKeys: Set<String> = [
        "backend", "mode", "network", "bridge_interface",
    ]

    private static func extraNicLines(in block: String) -> [String] {
        var extras: [String] = []
        for raw in block.split(omittingEmptySubsequences: false, whereSeparator: \.isNewline) {
            let line = String(raw)
            let trimmed = line.trimmingCharacters(in: .whitespaces)
            if trimmed.isEmpty || trimmed.hasPrefix("#") || trimmed.hasPrefix("[") {
                continue
            }
            let key = trimmed.split(separator: "=", maxSplits: 1).first?
                .trimmingCharacters(in: .whitespaces) ?? ""
            if nicCoreKeys.contains(key) {
                continue
            }
            extras.append(trimmed)
        }
        return extras
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

    /// True when the guest should try an x86 usermode engine (`auto` or a named engine, or `rosetta = true`).
    static func parseRunX86(in text: String) -> Bool {
        if bool(in: text, key: "rosetta") == true {
            return true
        }
        guard let value = optionalString(in: text, key: "x86_emulation") else {
            return false
        }
        return value != "none"
    }

    /// Toggle on → `x86_emulation = "auto"`; off → `none` and `rosetta = false`.
    static func replacingRunX86(in text: String, enabled: Bool) -> String {
        var out = upsertString(in: text, key: "x86_emulation", value: enabled ? "auto" : "none")
        if enabled {
            if bool(in: out, key: "rosetta") == false {
                out = removeKey(in: out, key: "rosetta")
            }
        } else {
            out = upsertBool(in: out, key: "rosetta", value: false)
        }
        return out
    }

    private static func bool(in text: String, key: String) -> Bool? {
        let pattern = #"^\s*"# + key + #"\s*=\s*(true|false)\s*$"#
        guard let regex = try? NSRegularExpression(pattern: pattern, options: [.anchorsMatchLines]) else {
            return nil
        }
        let range = NSRange(text.startIndex..<text.endIndex, in: text)
        guard let match = regex.firstMatch(in: text, range: range),
              let capture = Range(match.range(at: 1), in: text) else {
            return nil
        }
        return String(text[capture]) == "true"
    }

    private static func upsertString(in text: String, key: String, value: String) -> String {
        upsertLine(in: text, key: key, line: "\(key) = \"\(value)\"")
    }

    private static func upsertBool(in text: String, key: String, value: Bool) -> String {
        upsertLine(in: text, key: key, line: "\(key) = \(value ? "true" : "false")")
    }

    private static func upsertLine(in text: String, key: String, line: String) -> String {
        let pattern = #"^\s*"# + NSRegularExpression.escapedPattern(for: key) + #"\s*=.*$"#
        guard let regex = try? NSRegularExpression(pattern: pattern, options: [.anchorsMatchLines]) else {
            return text + "\n" + line + "\n"
        }
        let range = NSRange(text.startIndex..<text.endIndex, in: text)
        if regex.firstMatch(in: text, range: range) != nil {
            return regex.stringByReplacingMatches(in: text, options: [], range: range, withTemplate: line)
        }
        let trimmed = text.trimmingCharacters(in: .newlines)
        return trimmed + "\n" + line + "\n"
    }

    private static func removeKey(in text: String, key: String) -> String {
        let pattern = #"^\s*"# + NSRegularExpression.escapedPattern(for: key) + #"\s*=.*\n?"#
        guard let regex = try? NSRegularExpression(pattern: pattern, options: [.anchorsMatchLines]) else {
            return text
        }
        let range = NSRange(text.startIndex..<text.endIndex, in: text)
        return regex.stringByReplacingMatches(in: text, options: [], range: range, withTemplate: "")
    }

    /// `[audio] input` (default false).
    static func parseAudioInput(in text: String) -> Bool {
        sectionBool(in: text, table: "audio", key: "input") ?? false
    }

    /// `[camera] enabled` (default false).
    static func parseCameraEnabled(in text: String) -> Bool {
        sectionBool(in: text, table: "camera", key: "enabled") ?? false
    }

    /// `[usb] enabled` (default true).
    static func parseUsbEnabled(in text: String) -> Bool {
        sectionBool(in: text, table: "usb", key: "enabled") ?? true
    }

    /// `[vsock] agent` (default true when the table exists).
    static func parseClipboardAgent(in text: String) -> Bool {
        guard text.contains("[vsock]") else { return false }
        return sectionBool(in: text, table: "vsock", key: "agent") ?? true
    }

    static func parseShares(in text: String) -> [(tag: String, hostPath: String, readOnly: Bool)] {
        var shares: [(String, String, Bool)] = []
        let parts = text.components(separatedBy: "[[shares]]")
        for (index, part) in parts.enumerated() {
            if index == 0 { continue }
            let block = part.components(separatedBy: "[[").first ?? part
            let tag = tableString(in: block, key: "tag") ?? ""
            let host = tableString(in: block, key: "host_path") ?? ""
            let ro = tableBoolLine(in: block, key: "read_only") ?? false
            shares.append((tag, host, ro))
        }
        return shares
    }

    static func replacingAudioInput(in text: String, enabled: Bool) -> String {
        upsertTableBool(in: text, table: "audio", key: "input", value: enabled)
    }

    static func replacingCameraEnabled(in text: String, enabled: Bool) -> String {
        upsertTableBool(in: text, table: "camera", key: "enabled", value: enabled)
    }

    private static func sectionBody(in text: String, table: String) -> String? {
        let marker = "[\(table)]"
        guard let range = text.range(of: marker) else { return nil }
        let rest = text[range.upperBound...]
        if let next = rest.range(of: "\n[") {
            return String(rest[..<next.lowerBound])
        }
        return String(rest)
    }

    private static func sectionBool(in text: String, table: String, key: String) -> Bool? {
        guard let body = sectionBody(in: text, table: table) else { return nil }
        return tableBoolLine(in: body, key: key)
    }

    private static func tableBoolLine(in text: String, key: String) -> Bool? {
        bool(in: text, key: key)
    }

    private static func upsertTableBool(in text: String, table: String, key: String, value: Bool)
        -> String
    {
        let line = "\(key) = \(value ? "true" : "false")"
        let marker = "[\(table)]"
        if let body = sectionBody(in: text, table: table) {
            let pattern = #"^\s*"# + NSRegularExpression.escapedPattern(for: key) + #"\s*=.*$"#
            if let regex = try? NSRegularExpression(pattern: pattern, options: [.anchorsMatchLines])
            {
                let range = NSRange(body.startIndex..<body.endIndex, in: body)
                if regex.firstMatch(in: body, range: range) != nil {
                    let newBody = regex.stringByReplacingMatches(
                        in: body, options: [], range: range, withTemplate: line)
                    if let markerRange = text.range(of: marker) {
                        let start = markerRange.upperBound
                        let end = text.index(start, offsetBy: body.count)
                        var out = text
                        out.replaceSubrange(start..<end, with: newBody)
                        return out
                    }
                }
            }
            if let markerRange = text.range(of: marker) {
                var out = text
                out.insert(contentsOf: "\n" + line, at: markerRange.upperBound)
                return out
            }
        }
        let trimmed = text.trimmingCharacters(in: .newlines)
        return trimmed + "\n\n[\(table)]\n\(line)\n"
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
