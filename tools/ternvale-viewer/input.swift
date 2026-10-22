// Keyboard and pointer capture for the dev viewer (wire version 2).
//
// Sends KEY / POINTER / BUTTON / SCROLL messages (present_protocol.rs). Keys
// travel as macOS virtual key codes; the VMM maps them to Linux KEY_* codes
// and feeds its virtio-input keyboard. The pointer is absolute: the position
// inside the aspect-fit frame becomes 0...32767 on each axis, so the guest's
// cursor sits under the host cursor without grabbing or warping it.

import AppKit

let kindKey: UInt16 = 0x0201
let kindPointer: UInt16 = 0x0202
let kindButton: UInt16 = 0x0203
let kindScroll: UInt16 = 0x0204
let kindSetSize: UInt16 = 0x0205
let pointerMax = 32767.0

/// Device-dependent modifier bits (IOKit `NX_DEVICE*KEYMASK`) by key code, so
/// left and right modifiers are told apart in `flagsChanged`.
let modifierMasks: [UInt16: UInt] = [
    0x3B: 0x0001,  // left control
    0x38: 0x0002,  // left shift
    0x3C: 0x0004,  // right shift
    0x37: 0x0008,  // left command
    0x36: 0x0010,  // right command
    0x3A: 0x0020,  // left option
    0x3D: 0x0040,  // right option
    0x3E: 0x2000,  // right control
]
let capsLockKey: UInt16 = 0x39
/// Points of precise (trackpad) scrolling per wheel click.
let pointsPerClick: CGFloat = 10

/// Encodes input messages and remembers what is held, so focus loss can
/// release it instead of leaving keys stuck down in the guest.
final class InputSender {
    let link: Link
    private var keysDown = Set<UInt16>()
    private var buttonsDown = Set<UInt64>()

    init(link: Link) { self.link = link }

    func key(_ code: UInt16, _ pressed: Bool) {
        if pressed {
            keysDown.insert(code)
        } else if keysDown.remove(code) == nil {
            return  // pressed before the window had focus
        }
        send(kindKey, UInt64(code) | (pressed ? 1 << 16 : 0))
    }

    /// 0 left, 1 right, 2 middle.
    func button(_ index: UInt64, _ pressed: Bool) {
        if pressed {
            buttonsDown.insert(index)
        } else if buttonsDown.remove(index) == nil {
            return
        }
        send(kindButton, index | (pressed ? 1 << 8 : 0))
    }

    /// `x`, `y` in 0...1 from the frame's top-left; clamped.
    func pointer(x: Double, y: Double) {
        let px = UInt64((min(max(x, 0), 1) * pointerMax).rounded())
        let py = UInt64((min(max(y, 0), 1) * pointerMax).rounded())
        send(kindPointer, px | py << 32)
    }

    /// Wheel clicks: `dx > 0` right, `dy > 0` up.
    func scroll(dx: Int32, dy: Int32) {
        send(kindScroll, UInt64(UInt32(bitPattern: dx)) | UInt64(UInt32(bitPattern: dy)) << 32)
    }

    /// Host window resize (TVDP `SET_SIZE`); same as the Swift app's `VMDisplayPanel`.
    func requestHostMode(width: UInt32, height: UInt32) {
        send(kindSetSize, UInt64(width) | UInt64(height) << 32)
    }

    func releaseAll() {
        for code in keysDown { key(code, false) }
        for index in buttonsDown { button(index, false) }
    }

    private func send(_ kind: UInt16, _ value: UInt64) {
        // A dead link ends the reader thread, which quits the app.
        if !link.send(kind: kind, value: value) { warn("input not sent; presenter went away") }
    }
}

/// `--headless --script-input`: a fixed input sequence for the wire test.
func sendScriptedInput(_ input: InputSender) {
    input.key(0x00, true)
    input.key(0x00, false)
    input.pointer(x: 0.5, y: 0.25)
    input.button(0, true)
    input.button(0, false)
    input.scroll(dx: 0, dy: 1)
}

/// The window's content: shows frames (layer contents) and captures input.
final class SurfaceView: NSView {
    var input: InputSender?
    /// Guest frame size in pixels; zero until the first scanout.
    var frameSize = CGSize.zero
    private var tracking: NSTrackingArea?
    private var scrollAccum = CGPoint.zero

    override var acceptsFirstResponder: Bool { true }
    override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let old = tracking { removeTrackingArea(old) }
        let area = NSTrackingArea(rect: .zero, options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect],
                                  owner: self, userInfo: nil)
        addTrackingArea(area)
        tracking = area
    }

    override func keyDown(with event: NSEvent) {
        if event.isARepeat { return }  // the guest autorepeats (EV_REP)
        input?.key(event.keyCode, true)
        // AppKit sends no keyUp for a key released while Command is held.
        if event.modifierFlags.contains(.command) { input?.key(event.keyCode, false) }
    }

    override func keyUp(with event: NSEvent) { input?.key(event.keyCode, false) }

    override func flagsChanged(with event: NSEvent) {
        if event.keyCode == capsLockKey {
            // Reported once per toggle; Linux toggles on press.
            input?.key(capsLockKey, true)
            input?.key(capsLockKey, false)
            return
        }
        guard let mask = modifierMasks[event.keyCode] else { return }
        input?.key(event.keyCode, event.modifierFlags.rawValue & mask != 0)
    }

    private func move(_ event: NSEvent) {
        guard frameSize.width > 0, frameSize.height > 0, let input else { return }
        let p = convert(event.locationInWindow, from: nil)
        let scale = min(bounds.width / frameSize.width, bounds.height / frameSize.height)
        let (w, h) = (frameSize.width * scale, frameSize.height * scale)
        let (ox, oy) = ((bounds.width - w) / 2, (bounds.height - h) / 2)
        input.pointer(x: Double((p.x - ox) / w), y: Double(1 - (p.y - oy) / h))
    }

    override func mouseMoved(with event: NSEvent) { move(event) }
    override func mouseDragged(with event: NSEvent) { move(event) }
    override func rightMouseDragged(with event: NSEvent) { move(event) }
    override func otherMouseDragged(with event: NSEvent) { move(event) }
    override func mouseDown(with event: NSEvent) { move(event); input?.button(0, true) }
    override func mouseUp(with event: NSEvent) { input?.button(0, false) }
    override func rightMouseDown(with event: NSEvent) { move(event); input?.button(1, true) }
    override func rightMouseUp(with event: NSEvent) { input?.button(1, false) }
    override func otherMouseDown(with event: NSEvent) {
        if event.buttonNumber == 2 { move(event); input?.button(2, true) }
    }
    override func otherMouseUp(with event: NSEvent) {
        if event.buttonNumber == 2 { input?.button(2, false) }
    }

    override func scrollWheel(with event: NSEvent) {
        var dx = 0, dy = 0
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
        // AppKit's positive deltaY already means "scroll up" (REL_WHEEL +1);
        // positive deltaX means content moves right, i.e. scroll left.
        // TODO(verify) against a guest with natural scrolling turned off.
        if dx != 0 || dy != 0 { input?.scroll(dx: Int32(-dx), dy: Int32(dy)) }
    }
}
