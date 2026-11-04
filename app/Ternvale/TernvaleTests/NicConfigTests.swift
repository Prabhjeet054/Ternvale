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
                    bridgeInterface: ""
                )
            ]
        )
        XCTAssertTrue(next.contains("kernel = \"/path/Image\""), next)
        XCTAssertTrue(next.contains("mode = \"host-only\""), next)
        XCTAssertFalse(next.contains("backend = \"loopback\""), next)
    }

    func testBridgedEntitlementCopyIsExplicit() {
        XCTAssertNotNil(VmnetHelperService.messageIfBridgedDenied("vmnet_start_interface: NOT_AUTHORIZED"))
        XCTAssertNotNil(VmnetHelperService.messageIfBridgedDenied("needs com.apple.vm.networking"))
        XCTAssertNil(VmnetHelperService.messageIfBridgedDenied("helper down"))
        XCTAssertTrue(VmnetHelperService.bridgedEntitlementMissing.contains("com.apple.vm.networking"))
        XCTAssertTrue(VmnetHelperService.bridgedEntitlementMissing.contains("docs/ENTITLEMENT_REQUEST.md"))
    }
}
