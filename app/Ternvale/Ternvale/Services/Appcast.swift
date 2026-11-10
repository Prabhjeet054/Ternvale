import CryptoKit
import Foundation
import os

/// Sparkle 2 appcast (`xmlns:sparkle`). Signature bytes live on the enclosure;
/// the updater also requires Developer ID + notarization of the zip.
struct SparkleAppcast: Equatable {
    struct Item: Equatable {
        let shortVersion: String
        let build: String
        /// `nil` is the stable channel.
        let channel: String?
        let enclosureURL: String
        let edSignature: String
        let length: Int
    }

    enum ParseError: Error, Equatable {
        case notRss
        case missingEnclosure
        case missingEdSignature
        case emptySignature
    }

    let items: [Item]

    static func parse(_ xml: String) throws -> SparkleAppcast {
        let blocks = xml.components(separatedBy: "<item>").dropFirst()
        var items: [Item] = []
        for block in blocks {
            let short = tag("sparkle:shortVersionString", in: block)
                ?? tag("title", in: block)
                ?? ""
            let build = tag("sparkle:version", in: block) ?? "0"
            let channel = tag("sparkle:channel", in: block)
            guard let enclosure = enclosureAttributes(in: block) else {
                throw ParseError.missingEnclosure
            }
            let sig = enclosure["sparkle:edSignature"] ?? enclosure["edSignature"] ?? ""
            if sig.isEmpty { throw ParseError.missingEdSignature }
            let length = Int(enclosure["length"] ?? "0") ?? 0
            let url = enclosure["url"] ?? ""
            items.append(
                Item(
                    shortVersion: short,
                    build: build,
                    channel: channel,
                    enclosureURL: url,
                    edSignature: sig,
                    length: length
                )
            )
        }
        if items.isEmpty { throw ParseError.notRss }
        return SparkleAppcast(items: items)
    }

    /// Tampered or unsigned enclosures are rejected before Sparkle would install.
    static func requireSignatures(_ appcast: SparkleAppcast) throws {
        for item in appcast.items {
            if item.edSignature.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                throw ParseError.emptySignature
            }
        }
    }

    /// Sparkle 2 Ed25519 over the raw archive bytes (`sparkle:edSignature` + `SUPublicEDKey`).
    static func verifyArchive(
        _ archive: Data,
        edSignature: String,
        publicKeyBase64: String
    ) -> Bool {
        let log = Logger(subsystem: "com.ternvale.app", category: "sparkle")
        guard let sig = Data(base64Encoded: edSignature.trimmingCharacters(in: .whitespacesAndNewlines)),
              let pub = Data(base64Encoded: publicKeyBase64.trimmingCharacters(in: .whitespacesAndNewlines))
        else {
            log.error("sparkle ed25519 decode failed")
            return false
        }
        do {
            let key = try Curve25519.Signing.PublicKey(rawRepresentation: pub)
            let ok = key.isValidSignature(sig, for: archive)
            if ok {
                log.info("sparkle ed25519 ok bytes=\(archive.count, privacy: .public)")
            } else {
                log.error("sparkle ed25519 rejected bytes=\(archive.count, privacy: .public)")
            }
            return ok
        } catch {
            log.error("sparkle ed25519 key: \(error.localizedDescription, privacy: .public)")
            return false
        }
    }

    /// True when the appcast item is newer than `current` and the zip verifies.
    static func shouldInstall(
        item: Item,
        archive: Data,
        currentVersion: String,
        publicKeyBase64: String
    ) -> Bool {
        guard SemVer.isNewer(item.shortVersion, than: currentVersion) else { return false }
        return verifyArchive(archive, edSignature: item.edSignature, publicKeyBase64: publicKeyBase64)
    }

    static func feedURL(channel: String) -> URL? {
        if let env = ProcessInfo.processInfo.environment["TERNVALE_APPCAST_URL"], !env.isEmpty {
            return URL(string: env)
        }
        let stable = "https://updates.ternvale.dev/appcast.xml"
        let beta = "https://updates.ternvale.dev/appcast-beta.xml"
        return URL(string: channel == "beta" ? beta : stable)
    }

    private static func tag(_ name: String, in xml: String) -> String? {
        let open = "<\(name)>"
        let close = "</\(name)>"
        guard let a = xml.range(of: open), let b = xml.range(of: close) else { return nil }
        return String(xml[a.upperBound ..< b.lowerBound]).trimmingCharacters(in: .whitespacesAndNewlines)
    }

    private static func enclosureAttributes(in xml: String) -> [String: String]? {
        guard let start = xml.range(of: "<enclosure") else { return nil }
        let rest = xml[start.lowerBound...]
        guard let end = rest.range(of: "/>") ?? rest.range(of: ">") else { return nil }
        let tag = String(rest[..<end.upperBound])
        var attrs: [String: String] = [:]
        let pattern = #"([A-Za-z0-9:_-]+)="([^"]*)""#
        if let regex = try? NSRegularExpression(pattern: pattern) {
            let ns = tag as NSString
            for match in regex.matches(in: tag, range: NSRange(location: 0, length: ns.length)) {
                let key = ns.substring(with: match.range(at: 1))
                let value = ns.substring(with: match.range(at: 2))
                attrs[key] = value
            }
        }
        return attrs
    }
}
