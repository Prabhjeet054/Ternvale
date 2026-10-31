import XCTest
@testable import Ternvale

final class SeamlessWindowSyncTests: XCTestCase {
    func testRemovedIdsWhenGuestClosesOneApp() {
        let removed = SeamlessWindowSync.removedIds(current: [1, 2], incoming: [1])
        XCTAssertEqual(removed, [2])
    }

    func testAddedIdsWhenSecondAppOpens() {
        let added = SeamlessWindowSync.addedIds(current: [1], incoming: [1, 2])
        XCTAssertEqual(added, [2])
    }

    func testDecodeTwoWindowUpdate() throws {
        let json = """
        {"windows":[{"id":1,"title":"xterm","x":10,"y":20,"width":640,"height":480},{"id":2,"title":"gedit","x":700,"y":20,"width":800,"height":600}],"sequence":3}
        """
        struct Update: Decodable {
            struct Win: Decodable {
                let id: UInt32
                let title: String
            }
            let windows: [Win]
            let sequence: UInt64
        }
        let update = try JSONDecoder().decode(Update.self, from: Data(json.utf8))
        XCTAssertEqual(update.windows.count, 2)
        XCTAssertEqual(update.sequence, 3)
    }
}
