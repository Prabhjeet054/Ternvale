import Foundation
import os

/// Next-launch offer if the app process did not exit cleanly.
enum CrashHeartbeat {
    private static let log = Logger(subsystem: "com.ternvale.app", category: "crash")
    private static var launchOffer = false

    static var markerURL: URL {
        TernvalePaths.applicationSupport.appendingPathComponent("session.alive")
    }

    static func begin() {
        try? FileManager.default.createDirectory(
            at: TernvalePaths.applicationSupport,
            withIntermediateDirectories: true
        )
        try? "running".write(to: markerURL, atomically: true, encoding: .utf8)
    }

    static func endClean() {
        try? FileManager.default.removeItem(at: markerURL)
    }

    static func leftoverUncleanExit() -> Bool {
        FileManager.default.fileExists(atPath: markerURL.path)
    }

    /// Call once from `TernvaleApp.init` before writing a new marker.
    static func bootstrap() {
        launchOffer = leftoverUncleanExit()
        if launchOffer {
            try? FileManager.default.removeItem(at: markerURL)
            log.info("previous app session ended without a clean shutdown")
        }
        begin()
    }

    static func takeLaunchOffer() -> Bool {
        let value = launchOffer
        launchOffer = false
        return value
    }
}
