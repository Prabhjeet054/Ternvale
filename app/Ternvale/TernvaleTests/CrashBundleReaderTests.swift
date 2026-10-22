import XCTest
@testable import Ternvale

final class CrashBundleReaderTests: XCTestCase {
    func testSidecarPathMatchesRustStem() {
        let log = URL(fileURLWithPath: "/tmp/ternvale-demo-20261002-120000.log")
        let summary = CrashBundleReader.sidecarPath(hostLog: log, suffix: ".summary.txt")
        XCTAssertEqual(summary.lastPathComponent, "ternvale-demo-20261002-120000.summary.txt")
        XCTAssertEqual(
            CrashBundleReader.sidecarPath(hostLog: log, suffix: ".run.json").lastPathComponent,
            "ternvale-demo-20261002-120000.run.json"
        )
    }
}
