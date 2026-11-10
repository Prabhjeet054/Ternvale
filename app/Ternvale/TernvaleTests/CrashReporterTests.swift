import XCTest
@testable import Ternvale

private final class CaptureProtocol: URLProtocol, @unchecked Sendable {
    static var lastBody: Data?
    static var requestCount = 0

    static func reset() {
        lastBody = nil
        requestCount = 0
    }

    override class func canInit(with request: URLRequest) -> Bool { true }
    override class func canonicalRequest(for request: URLRequest) -> URLRequest { request }

    override func startLoading() {
        Self.requestCount += 1
        Self.lastBody = request.httpBody
            ?? request.httpBodyStream.flatMap { stream in
                let data = NSMutableData()
                let buf = UnsafeMutablePointer<UInt8>.allocate(capacity: 4096)
                defer { buf.deallocate() }
                stream.open()
                while stream.hasBytesAvailable {
                    let n = stream.read(buf, maxLength: 4096)
                    if n > 0 { data.append(buf, length: n) } else { break }
                }
                return data as Data
            }
        let response = HTTPURLResponse(
            url: request.url ?? URL(string: "http://127.0.0.1/v1/crash")!,
            statusCode: 201,
            httpVersion: "HTTP/1.1",
            headerFields: ["Content-Type": "application/json"]
        )!
        client?.urlProtocol(self, didReceive: response, cacheStoragePolicy: .notAllowed)
        client?.urlProtocol(self, didLoad: Data(#"{"ok":true}"#.utf8))
        client?.urlProtocolDidFinishLoading(self)
    }

    override func stopLoading() {}
}

final class CrashReporterTests: XCTestCase {
    func testSendRefusedWhenOptInDisabled() async {
        let payload = CrashReportPayload.preview(
            files: [(name: "a.log", bytes: 1, text: "x")],
            fields: [],
            appVersion: "0.1.0",
            redactor: { $0 }
        )
        do {
            try await CrashReporter.send(payload, optedIn: false)
            XCTFail("expected notOptedIn")
        } catch CrashReporter.Error.notOptedIn {
            // expected
        } catch {
            XCTFail("wrong error \(error)")
        }
    }

    func testAutoInstallDefaultsOff() {
        UserDefaults.standard.removeObject(forKey: AppSettings.autoInstallUpdatesKey)
        UserDefaults.standard.removeObject(forKey: AppSettings.crashReportingOptInKey)
        XCTAssertFalse(AppSettings.autoInstallUpdates)
        XCTAssertFalse(AppSettings.crashReportingOptIn)
        XCTAssertEqual(AppSettings.updateChannel, "stable")
    }

    func testPostedBodyMatchesPreview() async throws {
        CaptureProtocol.reset()
        let payload = CrashReportPayload.preview(
            files: [
                (name: "ternvale-demo.log", bytes: 12, text: "/Users/alice/x"),
                (name: "guest-serial.log", bytes: 99, text: "login: root"),
            ],
            fields: [("vm_name", "demo")],
            appVersion: "0.1.0",
            redactor: { CrashRedactor.redact($0, home: "/Users/alice", user: "alice", host: "Mac") }
        )
        XCTAssertFalse(payload.includedItems.contains { $0.name == "guest-serial.log" })
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [CaptureProtocol.self]
        let session = URLSession(configuration: config)
        try await CrashReporter.send(payload, session: session, optedIn: true)
        XCTAssertEqual(CaptureProtocol.requestCount, 1)
        let sentData = try XCTUnwrap(CaptureProtocol.lastBody)
        XCTAssertEqual(sentData, try payload.jsonData())
        let sent = try JSONSerialization.jsonObject(with: sentData)
        let files = (sent as? [String: Any])?["files"] as? [[String: Any]]
        XCTAssertEqual(files?.count, 1)
        XCTAssertEqual(files?.first?["name"] as? String, "ternvale-demo.log")
    }

    func testOptInDisabledMakesNoRequest() async {
        CaptureProtocol.reset()
        let payload = CrashReportPayload.preview(
            files: [(name: "a.log", bytes: 1, text: "x")],
            fields: [],
            appVersion: "0.1.0",
            redactor: { $0 }
        )
        let config = URLSessionConfiguration.ephemeral
        config.protocolClasses = [CaptureProtocol.self]
        let session = URLSession(configuration: config)
        do {
            try await CrashReporter.send(payload, session: session, optedIn: false)
            XCTFail("expected notOptedIn")
        } catch CrashReporter.Error.notOptedIn {
            XCTAssertEqual(CaptureProtocol.requestCount, 0)
            XCTAssertNil(CaptureProtocol.lastBody)
        } catch {
            XCTFail("wrong error \(error)")
        }
    }
}
