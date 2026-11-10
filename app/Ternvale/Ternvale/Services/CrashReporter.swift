import Foundation
import os

/// POSTs a crash payload only after opt-in **and** an explicit send from the preview.
enum CrashReporter {
    static let log = Logger(subsystem: "com.ternvale.app", category: "crash")

    enum Error: LocalizedError {
        case notOptedIn
        case noEndpoint
        case http(Int)

        var errorDescription: String? {
            switch self {
            case .notOptedIn:
                "Crash reporting is off. Nothing was sent."
            case .noEndpoint:
                "Crash endpoint URL is missing."
            case let .http(code):
                "Crash endpoint HTTP \(code)"
            }
        }
    }

    static func endpoint() -> URL? {
        if let env = ProcessInfo.processInfo.environment["TERNVALE_CRASH_URL"], !env.isEmpty {
            return URL(string: env)
        }
        if let plist = Bundle.main.object(forInfoDictionaryKey: "TERNVALECrashURL") as? String,
           !plist.isEmpty
        {
            return URL(string: plist)
        }
        return URL(string: "https://crash.ternvale.dev/v1/crash")
    }

    /// Returns without I/O when opt-in is false.
    static func send(
        _ payload: CrashReportPayload,
        session: URLSession = .shared,
        optedIn: Bool = AppSettings.crashReportingOptIn
    ) async throws {
        guard optedIn else {
            log.info("crash send skipped: opt-in disabled")
            throw Error.notOptedIn
        }
        guard let url = endpoint() else { throw Error.noEndpoint }
        var request = URLRequest(url: url)
        request.httpMethod = "POST"
        request.setValue("application/json", forHTTPHeaderField: "Content-Type")
        request.httpBody = try payload.jsonData()
        request.timeoutInterval = 20
        log.info("posting crash report files=\(payload.includedItems.count, privacy: .public)")
        let (_, response) = try await session.data(for: request)
        if let http = response as? HTTPURLResponse, !(200 ..< 300).contains(http.statusCode) {
            throw Error.http(http.statusCode)
        }
    }
}
