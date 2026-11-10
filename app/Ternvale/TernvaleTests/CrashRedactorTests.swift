import XCTest
@testable import Ternvale

final class CrashRedactorTests: XCTestCase {
    func testRedactsHomeUserAndHost() {
        let raw = "/Users/alice/Library/Logs/Ternvale/ternvale-demo.log on MacBook.local user=alice"
        let out = CrashRedactor.redact(raw, home: "/Users/alice", user: "alice", host: "MacBook.local")
        XCTAssertFalse(out.contains("alice"))
        XCTAssertFalse(out.contains("/Users/alice"))
        XCTAssertFalse(out.contains("MacBook.local"))
        XCTAssertTrue(out.contains("~") || out.contains("/Users/USER"))
        XCTAssertTrue(out.contains("HOST"))
    }

    func testExcludesSerialAndGuestDisks() {
        XCTAssertTrue(CrashRedactor.shouldExclude(filename: "demo.serial.log"))
        XCTAssertTrue(CrashRedactor.shouldExclude(filename: "guest-serial.txt"))
        XCTAssertTrue(CrashRedactor.shouldExclude(filename: "disk0.qcow2"))
        XCTAssertTrue(CrashRedactor.shouldExclude(filename: "rootfs.img"))
        XCTAssertFalse(CrashRedactor.shouldExclude(filename: "ternvale-demo.summary.txt"))
        XCTAssertFalse(CrashRedactor.shouldExclude(filename: "ternvale-demo.run.json"))
    }

    func testPreviewOmitsSerialAndListsFields() throws {
        let payload = CrashReportPayload.preview(
            files: [
                (name: "host.log", bytes: 12, text: "/Users/bob/Library/Logs/x"),
                (name: "guest-serial.log", bytes: 99, text: "login: root"),
            ],
            fields: [("os_version", "macOS 15")],
            appVersion: "0.1.0",
            redactor: { CrashRedactor.redact($0, home: "/Users/bob", user: "bob", host: "host") }
        )
        XCTAssertEqual(payload.includedItems.count, 2)
        XCTAssertTrue(payload.items.contains { $0.name == "guest-serial.log" && !$0.included })
        XCTAssertFalse((payload.includedItems.first?.redactedPreview ?? "").contains("/Users/bob"))
        let json = try JSONSerialization.jsonObject(with: payload.jsonData()) as? [String: Any]
        let files = json?["files"] as? [[String: Any]]
        XCTAssertEqual(files?.count, 1)
        XCTAssertEqual(files?.first?["name"] as? String, "host.log")
    }
}
