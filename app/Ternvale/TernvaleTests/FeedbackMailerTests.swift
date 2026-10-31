import XCTest
@testable import Ternvale

final class FeedbackMailerTests: XCTestCase {
    func testCollectsHostLogAndExistingSidecarsOnly() {
        let log = URL(fileURLWithPath: "/tmp/ternvale-demo-20261031-120000.log")
        let files = FeedbackMailer.collectSidecars(hostLog: log) { url in
            url.lastPathComponent.hasSuffix(".log") || url.lastPathComponent.hasSuffix(".summary.txt")
        }
        XCTAssertEqual(files.map(\.lastPathComponent).sorted(), [
            "ternvale-demo-20261031-120000.log",
            "ternvale-demo-20261031-120000.summary.txt",
        ])
    }

    func testMailtoHasRecipientSubjectAndNoRemoteUpload() throws {
        let url = try XCTUnwrap(
            FeedbackMailer.mailtoURL(
                to: "beta@ternvale.dev",
                subject: "Ternvale beta feedback — demo",
                body: "no upload"
            )
        )
        XCTAssertEqual(url.scheme, "mailto")
        XCTAssertTrue(url.absoluteString.contains("beta@ternvale.dev"))
        XCTAssertTrue(url.absoluteString.contains("subject="))
        XCTAssertFalse(url.absoluteString.contains("https://"))
    }

    func testMailBodyNamesTheZipAndDeniesUpload() {
        let body = FeedbackMailer.mailBody(vmName: "desktop", zipName: "ternvale-report-desktop.zip")
        XCTAssertTrue(body.contains("desktop"))
        XCTAssertTrue(body.contains("ternvale-report-desktop.zip"))
        XCTAssertTrue(body.contains("README.txt"))
        XCTAssertTrue(body.contains("does not upload"))
    }
}
