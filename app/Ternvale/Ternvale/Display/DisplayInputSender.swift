import AppKit
import Foundation
import os

/// virtio-input messages over TVDP (see `tools/ternvale-viewer/input.swift`).
final class DisplayInputSender {
    private let log = Logger(subsystem: "com.ternvale.app", category: "display.input")
    private weak var link: DisplaySocketLink?
    private var keysDown = Set<UInt16>()
    private var buttonsDown = Set<UInt64>()

    private let modifierMasks: [UInt16: UInt] = [
        0x3B: 0x0001, 0x38: 0x0002, 0x3C: 0x0004, 0x37: 0x0008,
        0x36: 0x0010, 0x3A: 0x0020, 0x3D: 0x0040, 0x3E: 0x2000,
    ]
    private let capsLockKey: UInt16 = 0x39
    private let pointsPerClick: CGFloat = 10
    private var scrollAccum = CGPoint.zero

    func attach(link: DisplaySocketLink) {
        self.link = link
    }

    func sendKey(_ code: UInt16, pressed: Bool) {
        if pressed {
            keysDown.insert(code)
        } else if keysDown.remove(code) == nil {
            return
        }
        send(kind: PresentWire.kindKey, value: UInt64(code) | (pressed ? 1 << 16 : 0))
    }

    func sendButton(_ index: UInt64, pressed: Bool) {
        if pressed {
            buttonsDown.insert(index)
        } else if buttonsDown.remove(index) == nil {
            return
        }
        send(kind: PresentWire.kindButton, value: index | (pressed ? 1 << 8 : 0))
    }

    func pointer(x: Double, y: Double) {
        let px = UInt64((min(max(x, 0), 1) * PresentWire.pointerMax).rounded())
        let py = UInt64((min(max(y, 0), 1) * PresentWire.pointerMax).rounded())
        send(kind: PresentWire.kindPointer, value: px | py << 32)
    }

    func scroll(dx: Int32, dy: Int32) {
        send(
            kind: PresentWire.kindScroll,
            value: UInt64(UInt32(bitPattern: dx)) | UInt64(UInt32(bitPattern: dy)) << 32
        )
    }

    func releaseAll() {
        for code in keysDown { sendKey(code, pressed: false) }
        for index in buttonsDown { sendButton(index, pressed: false) }
    }

    func handleKeyDown(_ event: NSEvent) {
        if event.isARepeat { return }
        sendKey(event.keyCode, pressed: true)
        if event.modifierFlags.contains(.command) {
            sendKey(event.keyCode, pressed: false)
        }
    }

    func handleKeyUp(_ event: NSEvent) {
        sendKey(event.keyCode, pressed: false)
    }

    func handleFlagsChanged(_ event: NSEvent) {
        if event.keyCode == capsLockKey {
            sendKey(capsLockKey, pressed: true)
            sendKey(capsLockKey, pressed: false)
            return
        }
        guard let mask = modifierMasks[event.keyCode] else { return }
        sendKey(event.keyCode, pressed: event.modifierFlags.rawValue & mask != 0)
    }

    func handleScroll(_ event: NSEvent) {
        var dx = 0
        var dy = 0
        if event.hasPreciseScrollingDeltas {
            scrollAccum.x += event.scrollingDeltaX
            scrollAccum.y += event.scrollingDeltaY
            dx = Int(scrollAccum.x / pointsPerClick)
            dy = Int(scrollAccum.y / pointsPerClick)
            scrollAccum.x -= CGFloat(dx) * pointsPerClick
            scrollAccum.y -= CGFloat(dy) * pointsPerClick
        } else {
            dx = event.scrollingDeltaX > 0 ? 1 : (event.scrollingDeltaX < 0 ? -1 : 0)
            dy = event.scrollingDeltaY > 0 ? 1 : (event.scrollingDeltaY < 0 ? -1 : 0)
        }
        if dx != 0 || dy != 0 {
            scroll(dx: Int32(-dx), dy: Int32(dy))
        }
    }

    private func send(kind: UInt16, value: UInt64) {
        if let link, link.send(kind: kind, value: value) {
            return
        }
        log.warning("display input not sent; presenter closed")
    }
}
