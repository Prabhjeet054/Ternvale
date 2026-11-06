import XCTest
@testable import Ternvale

final class ToolsOutdatedTests: XCTestCase {
    func testStatusJsonFlagsOutdatedTools() throws {
        let json = """
        {"ok":true,"status":{"name":"win","state":"running","cpus":4,"uptime_ms":1,"tools_outdated":true,"tools_iso_agent":"0.1.1"}}
        """
        let response = try JSONDecoder().decode(ControlResponse.self, from: Data(json.utf8))
        XCTAssertEqual(response.status?.toolsOutdated, true)
        XCTAssertEqual(response.status?.toolsIsoAgent, "0.1.1")
    }

    func testStatusJsonOmitsOptionalToolsFields() throws {
        let json = """
        {"ok":true,"status":{"name":"win","state":"running","cpus":4,"uptime_ms":1}}
        """
        let response = try JSONDecoder().decode(ControlResponse.self, from: Data(json.utf8))
        XCTAssertNil(response.status?.toolsOutdated)
        XCTAssertNil(response.status?.toolsIsoAgent)
    }
}
