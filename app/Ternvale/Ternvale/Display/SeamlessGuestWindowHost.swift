import AppKit
import Foundation
import os

/// One native macOS window for a guest HWND / X11 window.
@MainActor
final class SeamlessGuestWindowHost: NSObject, NSWindowDelegate {
    let id: UInt32
    let window: NSWindow
    private let metalView: SeamlessCropMetalView
    private let send: (String) -> Void
    private let log = Logger(subsystem: "com.ternvale.app", category: "seamless")
    private var applyingGuest = false
    private var lastRecord: GuestWindowRecord

    init(record: GuestWindowRecord, guestSize: CGSize, send: @escaping (String) -> Void) {
        self.id = record.id
        self.send = send
        self.lastRecord = record
        metalView = SeamlessCropMetalView(crop: Self.cropRect(for: record))
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: Int(record.width), height: Int(record.height)),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        super.init()
        window.title = record.title.isEmpty ? "Guest \(record.id)" : record.title
        window.contentView = metalView
        window.isReleasedWhenClosed = false
        window.delegate = self
        window.collectionBehavior = [.moveToActiveSpace]
        position(record: record, guestSize: guestSize)
        applyState(record.state)
        window.makeKeyAndOrderFront(nil)
    }

    func update(record: GuestWindowRecord, guestSize: CGSize) {
        lastRecord = record
        applyingGuest = true
        window.title = record.title.isEmpty ? "Guest \(record.id)" : record.title
        let size = NSSize(width: Int(record.width), height: Int(record.height))
        metalView.frame = NSRect(origin: .zero, size: size)
        metalView.cropRect = Self.cropRect(for: record)
        position(record: record, guestSize: guestSize)
        applyState(record.state)
        applyingGuest = false
    }

    func attachParent(_ parent: NSWindow) {
        parent.addChildWindow(window, ordered: .above)
    }

    func submit(frame: GuestDisplayFrame) {
        metalView.submit(frame: frame)
    }

    func submitCapture(width: UInt32, height: UInt32, rgb24: Data) {
        metalView.submitRgb24(width: width, height: height, pixels: rgb24)
    }

    func close() {
        window.delegate = nil
        window.orderOut(nil)
        window.close()
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        sendOp("close")
        return true
    }

    func windowDidMiniaturize(_ notification: Notification) {
        if !applyingGuest { sendOp("minimize") }
    }

    func windowDidDeminiaturize(_ notification: Notification) {
        if !applyingGuest { sendOp("restore") }
    }

    func windowDidBecomeKey(_ notification: Notification) {
        if !applyingGuest { sendOp("focus") }
    }

    func windowDidResize(_ notification: Notification) {
        guard !applyingGuest else { return }
        let f = window.frame
        send(
            #"{"op":"resize","id":\#(id),"width":\#(UInt32(max(1, f.width))),"height":\#(UInt32(max(1, f.height)))}"#
        )
    }

    func windowDidMove(_ notification: Notification) {
        guard !applyingGuest else { return }
        let f = window.frame
        send(
            #"{"op":"move","id":\#(id),"x":\#(Int32(f.origin.x)),"y":\#(Int32(f.origin.y)),"width":\#(UInt32(max(1, f.width))),"height":\#(UInt32(max(1, f.height)))}"#
        )
    }

    private func sendOp(_ op: String) {
        send(#"{"op":"\#(op)","id":\#(id)}"#)
        log.debug("host window_op \(op, privacy: .public) id=\(self.id, privacy: .public)")
    }

    private func applyState(_ state: String?) {
        switch state {
        case "minimized":
            if !window.isMiniaturized { window.miniaturize(nil) }
        case "maximized":
            if !window.isZoomed { window.zoom(nil) }
        default:
            if window.isMiniaturized { window.deminiaturize(nil) }
        }
    }

    private func position(record: GuestWindowRecord, guestSize: CGSize) {
        guard let screen = NSScreen.main else { return }
        let screenFrame = screen.frame
        var x = CGFloat(record.x)
        var y = CGFloat(record.y)
        let w = CGFloat(record.width)
        let h = CGFloat(record.height)
        if guestSize.width > 0, guestSize.height > 0 {
            let sx = screenFrame.width / guestSize.width
            let sy = screenFrame.height / guestSize.height
            x *= sx
            y *= sy
        }
        window.setFrame(
            NSRect(
                x: screenFrame.origin.x + x,
                y: screenFrame.origin.y + screenFrame.height - y - h,
                width: w,
                height: h
            ),
            display: true
        )
    }

    private static func cropRect(for record: GuestWindowRecord) -> CGRect {
        CGRect(
            x: CGFloat(record.x),
            y: CGFloat(record.y),
            width: CGFloat(record.width),
            height: CGFloat(record.height)
        )
    }
}
