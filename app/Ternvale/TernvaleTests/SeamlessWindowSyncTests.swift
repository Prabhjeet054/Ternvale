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

    func testTaskbarIsNotHostMapped() {
        XCTAssertFalse(SeamlessWindowSync.isHostMapped("taskbar"))
        XCTAssertFalse(SeamlessWindowSync.isHostMapped("start"))
        XCTAssertTrue(SeamlessWindowSync.isHostMapped("dialog"))
        XCTAssertTrue(SeamlessWindowSync.isHostMapped(nil))
    }

    func testDecodeWindowsUpdateWithParentAndKind() throws {
        let json = """
        {"windows":[{"id":2,"title":"Open","x":10,"y":20,"width":320,"height":240,"parent_id":1,"kind":"dialog","state":"normal","focused":true,"z_order":1}],"sequence":4}
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        struct Update: Decodable {
            let windows: [GuestWindowRecord]
        }
        let wrapped = try decoder.decode(Update.self, from: Data(json.utf8))
        XCTAssertEqual(wrapped.windows[0].parentId, 1)
        XCTAssertEqual(wrapped.windows[0].kind, "dialog")
        XCTAssertEqual(wrapped.windows[0].id, 2)
    }
}
