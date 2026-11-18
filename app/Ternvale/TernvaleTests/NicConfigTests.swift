import XCTest
@testable import Ternvale

final class NicConfigTests: XCTestCase {
    func testParseHostOnlyAndBridged() {
        let text = """
        name = "lab"
        cpus = 1
        ram_mib = 256
        serial_log = "/tmp/s.log"

        [[nics]]
        backend = "vmnet"
        mode = "host-only"
        network = "lab"

        [[nics]]
        backend = "vmnet"
        mode = "bridged"
        bridge_interface = "en0"
        """
        let nics = VMConfigParser.parseNics(in: text)
        XCTAssertEqual(nics.count, 2)
        XCTAssertEqual(nics[0].mode, "host-only")
        XCTAssertEqual(nics[0].network, "lab")
        XCTAssertEqual(nics[1].bridgeInterface, "en0")
    }

    func testReplaceNicsKeepsPolicyLines() {
        let text = """
        name = "lab"
        kernel = "/path/Image"

        [[nics]]
        backend = "user"
        allow_lan = false
        deny = [{ cidr = "10.0.0.0/8" }]
        """
        let nics = VMConfigParser.parseNics(in: text)
        XCTAssertEqual(nics[0].extraLines.contains("allow_lan = false"), true)
        let next = VMConfigParser.replacingNics(in: text, nics: nics)
        XCTAssertTrue(next.contains("allow_lan = false"), next)
        XCTAssertTrue(next.contains("deny = [{ cidr = \"10.0.0.0/8\" }]"), next)
    }

    func testReplaceNicsKeepsKernel() {
        let text = """
        name = "lab"
        kernel = "/path/Image"
        serial_log = "/tmp/s.log"

        [[nics]]
        backend = "loopback"
        """
        let next = VMConfigParser.replacingNics(
            in: text,
            nics: [
                NicConfig(
                    id: 0,
                    backend: "vmnet",
                    mode: "host-only",
                    network: "lab",
                    bridgeInterface: "",
                    extraLines: []
                )
            ]
        )
        XCTAssertTrue(next.contains("kernel = \"/path/Image\""), next)
        XCTAssertTrue(next.contains("mode = \"host-only\""), next)
        XCTAssertFalse(next.contains("backend = \"loopback\""), next)
    }

    func testRunX86ToggleWritesAutoAndNone() {
        let text = """
        name = "lab"
        kernel = "/path/Image"
        x86_emulation = "none"
        """
        XCTAssertFalse(VMConfigParser.parseRunX86(in: text))
        let on = VMConfigParser.replacingRunX86(in: text, enabled: true)
        XCTAssertTrue(on.contains("x86_emulation = \"auto\""), on)
        XCTAssertTrue(VMConfigParser.parseRunX86(in: on))
        let off = VMConfigParser.replacingRunX86(in: on, enabled: false)
        XCTAssertTrue(off.contains("x86_emulation = \"none\""), off)
        XCTAssertTrue(off.contains("rosetta = false"), off)
        XCTAssertFalse(VMConfigParser.parseRunX86(in: off))
    }

    func testRosettaTrueCountsAsRunX86() {
        let text = """
        name = "lab"
        rosetta = true
        """
        XCTAssertTrue(VMConfigParser.parseRunX86(in: text))
    }

    func testX86HelperFailureIncludesLogPointer() {
        let logs = TernvalePaths.hostLogDirectory.path
        let err = X86HelperError.failed(
            status: 1,
            stderr: "trace noise\nerror: x86 setup failed (host logs: \(logs)): cannot exec on vm deadx86: it is not running\n",
            logHint: logs
        )
        let text = err.localizedDescription
        XCTAssertTrue(text.contains("not running"), text)
        XCTAssertTrue(text.contains("Library/Logs/Ternvale"), text)
        let timeout = X86HelperError.timeout(logHint: logs)
        XCTAssertTrue(timeout.localizedDescription.contains("timed out"), timeout.localizedDescription)
        XCTAssertTrue(timeout.localizedDescription.contains("Host logs"), timeout.localizedDescription)
        XCTAssertEqual(X86PlanRunner.setupTimeout, 90, accuracy: 0.1)
    }

    func testBridgedEntitlementCopyIsExplicit() {
        XCTAssertNotNil(VmnetHelperService.messageIfBridgedDenied("vmnet_start_interface: NOT_AUTHORIZED"))
        XCTAssertNotNil(VmnetHelperService.messageIfBridgedDenied("needs com.apple.vm.networking"))
        XCTAssertNil(VmnetHelperService.messageIfBridgedDenied("helper down"))
        XCTAssertTrue(VmnetHelperService.bridgedEntitlementMissing.contains("com.apple.vm.networking"))
        XCTAssertTrue(VmnetHelperService.bridgedEntitlementMissing.contains("docs/ENTITLEMENT_REQUEST.md"))
    }
}
