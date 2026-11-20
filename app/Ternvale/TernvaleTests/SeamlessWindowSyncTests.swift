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

    func testNotepadAndCalculatorAreTwoMappedHostWindows() throws {
        let json = """
        {"windows":[{"id":256,"title":"Untitled - Notepad","x":10,"y":10,"width":400,"height":300},{"id":512,"title":"Calculator","x":80,"y":60,"width":320,"height":280}],"sequence":1}
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        struct Update: Decodable {
            let windows: [GuestWindowRecord]
        }
        let update = try decoder.decode(Update.self, from: Data(json.utf8))
        let mapped = update.windows.filter { SeamlessWindowSync.isHostMapped($0.kind) }
        XCTAssertEqual(mapped.map(\.id), [256, 512])
        XCTAssertTrue(mapped[0].x < mapped[1].x + Int32(mapped[1].width))
        XCTAssertEqual(SeamlessWindowSync.addedIds(current: [], incoming: Set(mapped.map(\.id))), [256, 512])
    }

    func testModalDialogAttachesToNotepadParent() throws {
        let json = """
        {"windows":[{"id":256,"title":"Untitled - Notepad","x":10,"y":10,"width":400,"height":300},{"id":257,"title":"Open","x":40,"y":40,"width":280,"height":180,"parent_id":256,"kind":"dialog"}],"sequence":2}
        """
        let decoder = JSONDecoder()
        decoder.keyDecodingStrategy = .convertFromSnakeCase
        struct Update: Decodable {
            let windows: [GuestWindowRecord]
        }
        let update = try decoder.decode(Update.self, from: Data(json.utf8))
        XCTAssertEqual(SeamlessWindowSync.childIds(parent: 256, windows: update.windows), [257])
    }

    func testToggleOffRemovesEveryHostWindow() {
        let removed = SeamlessWindowSync.removedIds(current: [256, 512], incoming: [])
        XCTAssertEqual(removed, [256, 512])
    }
}
