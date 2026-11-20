import XCTest

@testable import Ternvale

final class PrivacyConfigTests: XCTestCase {
    func testMicAndCameraDefaultOff() {
        let text = """
        name = "lab"
        cpus = 1
        ram_mib = 256
        """
        XCTAssertFalse(VMConfigParser.parseAudioInput(in: text))
        XCTAssertFalse(VMConfigParser.parseCameraEnabled(in: text))
        XCTAssertTrue(VMConfigParser.parseUsbEnabled(in: text))
        XCTAssertFalse(VMConfigParser.parseClipboardAgent(in: text))
        XCTAssertFalse(VMConfigParser.parseAgentLaunchers(in: text))
        XCTAssertFalse(VMConfigParser.parseAgentOpenInGuest(in: text))
        XCTAssertFalse(VMConfigParser.parseAgentOpenOnHost(in: text))
    }

    func testReplaceAgentAppsInsertsTable() {
        let text = """
        name = "lab"
        kernel = "/path/Image"
        """
        let on = VMConfigParser.replacingAgentLaunchers(in: text, enabled: true)
        XCTAssertTrue(on.contains("[agent]"), on)
        XCTAssertTrue(VMConfigParser.parseAgentLaunchers(in: on))
        let guest = VMConfigParser.replacingAgentOpenInGuest(in: on, enabled: true)
        XCTAssertTrue(VMConfigParser.parseAgentOpenInGuest(in: guest))
        let host = VMConfigParser.replacingAgentOpenOnHost(in: guest, enabled: true)
        XCTAssertTrue(VMConfigParser.parseAgentOpenOnHost(in: host))
    }

    func testReplaceAudioInputInsertsTable() {
        let text = """
        name = "lab"
        kernel = "/path/Image"
        """
        let on = VMConfigParser.replacingAudioInput(in: text, enabled: true)
        XCTAssertTrue(on.contains("[audio]"), on)
        XCTAssertTrue(on.contains("input = true"), on)
        XCTAssertTrue(VMConfigParser.parseAudioInput(in: on))
        let off = VMConfigParser.replacingAudioInput(in: on, enabled: false)
        XCTAssertTrue(off.contains("input = false"), off)
        XCTAssertFalse(VMConfigParser.parseAudioInput(in: off))
    }

    func testReplaceCameraKeepsKernel() {
        let text = """
        name = "lab"
        kernel = "/path/Image"

        [camera]
        enabled = false
        """
        let on = VMConfigParser.replacingCameraEnabled(in: text, enabled: true)
        XCTAssertTrue(on.contains("kernel = \"/path/Image\""), on)
        XCTAssertTrue(VMConfigParser.parseCameraEnabled(in: on))
    }

    func testParseShares() {
        let text = """
        [[shares]]
        tag = "host"
        host_path = "/Users/me/share"
        read_only = true
        """
        let shares = VMConfigParser.parseShares(in: text)
        XCTAssertEqual(shares.count, 1)
        XCTAssertEqual(shares[0].tag, "host")
        XCTAssertTrue(shares[0].readOnly)
    }
}
