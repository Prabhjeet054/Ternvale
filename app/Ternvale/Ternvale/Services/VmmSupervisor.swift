import Foundation
import os
import UserNotifications

/// Matches `ternvale_vmm::VmmSupervisor`: exponential backoff, stop after 3 crashes in 5 minutes.
final class VmmSupervisor {
    static let maxCrashes = 3
    static let window: TimeInterval = 5 * 60
    static let baseBackoff: TimeInterval = 1

    enum Decision: Equatable {
        case restart(backoff: TimeInterval, crashN: Int)
        case stop(reason: String, crashes: Int)
    }

    private var crashes: [TimeInterval] = []
    private var consecutive: Int = 0
    private let log = Logger(subsystem: "com.ternvale.app", category: "supervisor")

    func noteCrash(now: TimeInterval) -> Decision {
        crashes.append(now)
        crashes.removeAll { now - $0 > Self.window }
        consecutive += 1
        let n = crashes.count
        if n >= Self.maxCrashes {
            log.error("VMM process died 3 times in 5 minutes; stopping restarts crashes=\(n)")
            return .stop(reason: "VMM process died 3 times in 5 minutes", crashes: n)
        }
        let shift = min(max(consecutive - 1, 0), 6)
        let backoff = Self.baseBackoff * pow(2.0, Double(shift))
        log.error(
            "VMM process died unexpectedly; restarting after \(backoff, privacy: .public)s crashes=\(n)"
        )
        return .restart(backoff: backoff, crashN: n)
    }

    func noteHealthy() {
        consecutive = 0
    }

    static func markUncleanShutdown(vmBundle: URL, reason: String) {
        var data = Data()
        data.append(contentsOf: Array("TVUC".utf8))
        var version = UInt32(1).littleEndian
        data.append(Data(bytes: &version, count: 4))
        let ms = UInt64(Date().timeIntervalSince1970 * 1000).littleEndian
        var msLE = ms
        data.append(Data(bytes: &msLE, count: 8))
        let reasonBytes = Array(reason.utf8)
        var len = UInt32(reasonBytes.count).littleEndian
        data.append(Data(bytes: &len, count: 4))
        data.append(contentsOf: reasonBytes)
        let path = vmBundle.appendingPathComponent("unclean-shutdown")
        do {
            try data.write(to: path, options: .atomic)
            Logger(subsystem: "com.ternvale.app", category: "supervisor").error(
                "disk marked unclean-shutdown path=\(path.path, privacy: .public) reason=\(reason, privacy: .public)"
            )
        } catch {
            Logger(subsystem: "com.ternvale.app", category: "supervisor").error(
                "failed to write unclean-shutdown: \(error.localizedDescription, privacy: .public)"
            )
        }
    }

    static func notifyUser(title: String, body: String) {
        let content = UNMutableNotificationContent()
        content.title = title
        content.body = body
        let req = UNNotificationRequest(
            identifier: "ternvale.supervisor.\(UUID().uuidString)",
            content: content,
            trigger: nil
        )
        UNUserNotificationCenter.current().requestAuthorization(options: [.alert, .sound]) { _, _ in
            UNUserNotificationCenter.current().add(req, withCompletionHandler: nil)
        }
    }
}
