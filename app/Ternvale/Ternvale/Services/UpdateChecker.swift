import Foundation
import os

/// GET the hosted feed and compare it to this app's marketing version.
/// Does not download or install anything.
@MainActor
final class UpdateChecker: ObservableObject {
    @Published private(set) var banner: Banner?

    struct Banner: Equatable {
        let remoteVersion: String
        let currentVersion: String
        let notes: String?
        let learnMore: URL?
    }

    private let log = Logger(subsystem: "com.ternvale.app", category: "update")
    private let session: URLSession
    private let currentVersion: String

    init(
        session: URLSession = .shared,
        currentVersion: String = Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
            ?? "0.0.0"
    ) {
        self.session = session
        self.currentVersion = currentVersion
    }

    func check() {
        Task { await refresh() }
    }

    func dismiss() {
        if let version = banner?.remoteVersion {
            AppSettings.dismissedUpdateVersion = version
            log.info("dismissed update banner for \(version, privacy: .public)")
        }
        banner = nil
    }

    func refresh() async {
        guard let url = UpdateFeedURL.resolved() else {
            log.error("update feed URL is missing or invalid")
            return
        }
        log.info("checking for updates at \(url.absoluteString, privacy: .public)")
        var request = URLRequest(url: url)
        request.timeoutInterval = 15
        request.cachePolicy = .reloadIgnoringLocalCacheData
        do {
            let (data, response) = try await session.data(for: request)
            if let http = response as? HTTPURLResponse, !(200 ..< 300).contains(http.statusCode) {
                log.warning("update feed HTTP \(http.statusCode, privacy: .public)")
                return
            }
            let feed = try JSONDecoder().decode(UpdateFeed.self, from: data)
            apply(feed: feed)
        } catch {
            log.debug("update check skipped: \(error.localizedDescription, privacy: .public)")
        }
    }

    func apply(feed: UpdateFeed) {
        guard SemVer.isNewer(feed.version, than: currentVersion) else {
            log.debug(
                "up to date current=\(self.currentVersion, privacy: .public) feed=\(feed.version, privacy: .public)"
            )
            banner = nil
            return
        }
        if AppSettings.dismissedUpdateVersion == feed.version {
            log.debug("update \(feed.version, privacy: .public) already dismissed")
            banner = nil
            return
        }
        log.info(
            "new version available remote=\(feed.version, privacy: .public) current=\(self.currentVersion, privacy: .public)"
        )
        banner = Banner(
            remoteVersion: feed.version,
            currentVersion: currentVersion,
            notes: feed.notes,
            learnMore: feed.url.flatMap(URL.init(string:))
        )
    }
}
