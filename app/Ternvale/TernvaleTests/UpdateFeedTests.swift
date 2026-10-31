import XCTest
@testable import Ternvale

final class UpdateFeedTests: XCTestCase {
    func testNewerRemoteShowsUpdate() {
        XCTAssertTrue(SemVer.isNewer("0.2.0", than: "0.1.0"))
        XCTAssertTrue(SemVer.isNewer("1.0.0", than: "0.9.9"))
        XCTAssertFalse(SemVer.isNewer("0.1.0", than: "0.1.0"))
        XCTAssertFalse(SemVer.isNewer("0.1.0", than: "0.2.0"))
    }

    func testDecodeFeedJson() throws {
        let json = """
        {"version":"0.2.0","build":2,"notes":"fixes","url":"https://updates.ternvale.dev/"}
        """
        let feed = try JSONDecoder().decode(UpdateFeed.self, from: Data(json.utf8))
        XCTAssertEqual(feed.version, "0.2.0")
        XCTAssertEqual(feed.build, 2)
        XCTAssertEqual(feed.url, "https://updates.ternvale.dev/")
    }

    @MainActor
    func testCheckerSetsBannerWithoutDownloading() {
        AppSettings.dismissedUpdateVersion = nil
        let checker = UpdateChecker(currentVersion: "0.1.0")
        checker.apply(feed: UpdateFeed(version: "0.2.0", build: 2, notes: "n", url: "https://example.test/"))
        XCTAssertEqual(checker.banner?.remoteVersion, "0.2.0")
        XCTAssertEqual(checker.banner?.currentVersion, "0.1.0")
        checker.apply(feed: UpdateFeed(version: "0.1.0", build: 1, notes: nil, url: nil))
        XCTAssertNil(checker.banner)
    }
}
