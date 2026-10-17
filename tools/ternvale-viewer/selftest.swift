// Window self-test (dev only): with TERNVALE_VIEWER_SELFTEST=<script file>,
// the viewer plays the script against its own window once it is up. Keys and
// mouse moves become NSEvents handed to `NSWindow.sendEvent`, so they take the
// same path as hardware input from there on (window -> SurfaceView handlers ->
// InputSender -> presenter). The HID / WindowServer leg before the window is
// not exercised; that needs a person at the keyboard. The window is shown
// without taking focus, so real typing elsewhere does not mix in.
//
// Script lines (blank lines and `#` comments ignored):
//   wait-file <path>   until the file exists (the driving script's trigger)
//   wait-frame         until the first scanout has a size
//   sleep <ms>
//   type <text>        US layout; `\n` is Return, `\\` a backslash
//   move <gx> <gy>     pointer to the centre of guest pixel (gx, gy)
//   click              left button down + up where the pointer is
//   frame-info         prints window number and geometry (for screencapture -l)
// Every step is echoed to stderr as "ternvale-viewer: selftest ...".

import AppKit

let selftestEnv = "TERNVALE_VIEWER_SELFTEST"
let kvkShift: UInt16 = 0x38
/// NX_DEVICELSHIFTKEYMASK; SurfaceView.flagsChanged reads it (modifierMasks).
let leftShiftDeviceMask: UInt = 0x0002
let selftestKeyGap: useconds_t = 8_000

/// macOS virtual key codes (HIToolbox kVK_ANSI_*) indexed by code; NUL marks
/// codes that are not plain characters.
let plainKeys = Array("asdfhgzxcv\0bqweryt123465=97-80]ou[ip\nlj'k;\\,/nm.\t `")
let shiftedKeys = Array("ASDFHGZXCV\0BQWERYT!@#$^%+(&_*)}OU{IP\0LJ\"K:|<?NM>\0\0~")

/// (key code, needs shift) for a US-layout character.
func usKey(_ ch: Character) -> (UInt16, Bool)? {
    if ch == "\0" { return nil }
    if let i = plainKeys.firstIndex(of: ch) { return (UInt16(i), false) }
    if let i = shiftedKeys.firstIndex(of: ch) { return (UInt16(i), true) }
    return nil
}

func selftestNote(_ message: String) { warn("selftest \(message)") }

final class SelfTest {
    let window: NSWindow
    let surface: SurfaceView
    private var pointer = NSPoint.zero
    private var shiftDown = false

    init(window: NSWindow, surface: SurfaceView) {
        self.window = window
        self.surface = surface
    }

    static var scriptPath: String? {
        guard let path = ProcessInfo.processInfo.environment[selftestEnv], !path.isEmpty else { return nil }
        return path
    }

    static var requested: Bool { scriptPath != nil }

    /// Starts the script on a background thread if the env var names one.
    static func startIfRequested(window: NSWindow, surface: SurfaceView) {
        guard let path = scriptPath else { return }
        guard let script = try? String(contentsOfFile: path, encoding: .utf8) else {
            fail("cannot read \(selftestEnv)=\(path)")
        }
        let test = SelfTest(window: window, surface: surface)
        Thread { test.run(script) }.start()
    }

    func run(_ script: String) {
        for raw in script.split(separator: "\n", omittingEmptySubsequences: false) {
            let line = raw.trimmingCharacters(in: .whitespaces)
            if line.isEmpty || line.hasPrefix("#") { continue }
            let (cmd, arg) = split(line)
            switch cmd {
            case "wait-file":
                while !FileManager.default.fileExists(atPath: arg) { usleep(100_000) }
                selftestNote("wait-file done path=\(arg)")
            case "wait-frame":
                while onMain({ self.surface.frameSize.width == 0 }) { usleep(50_000) }
                let size = onMain { self.surface.frameSize }
                selftestNote("wait-frame done width=\(Int(size.width)) height=\(Int(size.height))")
            case "sleep":
                guard let ms = UInt32(arg) else { fail("selftest: bad sleep '\(arg)'") }
                usleep(ms * 1000)
            case "type": type(unescape(arg))
            case "move":
                let parts = arg.split(separator: " ").compactMap { Double($0) }
                guard parts.count == 2 else { fail("selftest: move needs <gx> <gy>, got '\(arg)'") }
                move(gx: parts[0], gy: parts[1])
            case "click": click()
            case "frame-info": onMain { self.frameInfo() }
            default: fail("selftest: unknown command '\(cmd)'")
            }
        }
        selftestNote("script done")
    }

    private func split(_ line: String) -> (String, String) {
        guard let space = line.firstIndex(of: " ") else { return (line, "") }
        return (String(line[..<space]), String(line[line.index(after: space)...]))
    }

    private func unescape(_ text: String) -> String {
        var out = ""
        var escaped = false
        for ch in text {
            if escaped {
                out.append(ch == "n" ? "\n" : ch)
                escaped = false
            } else if ch == "\\" {
                escaped = true
            } else {
                out.append(ch)
            }
        }
        return out
    }

    private func onMain<T>(_ body: @escaping () -> T) -> T {
        DispatchQueue.main.sync(execute: body)
    }

    private func type(_ text: String) {
        for ch in text {
            guard let (code, shift) = usKey(ch) else { fail("selftest: no US key for \(ch.debugDescription)") }
            if shift != shiftDown { setShift(shift) }
            let chars = String(ch)
            postKey(.keyDown, code, chars)
            postKey(.keyUp, code, chars)
        }
        if shiftDown { setShift(false) }
        selftestNote("typed chars=\(text.count) text=\(text.debugDescription)")
    }

    private func setShift(_ down: Bool) {
        shiftDown = down
        postKey(.flagsChanged, kvkShift, "")
    }

    private func flags() -> NSEvent.ModifierFlags {
        shiftDown ? NSEvent.ModifierFlags(rawValue: NSEvent.ModifierFlags.shift.rawValue | leftShiftDeviceMask) : []
    }

    private func postKey(_ type: NSEvent.EventType, _ code: UInt16, _ chars: String) {
        onMain {
            guard let event = NSEvent.keyEvent(
                with: type, location: .zero, modifierFlags: self.flags(),
                timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: self.window.windowNumber,
                context: nil, characters: chars, charactersIgnoringModifiers: chars,
                isARepeat: false, keyCode: code)
            else { fail("selftest: could not build key event code=\(code)") }
            self.window.sendEvent(event)
        }
        usleep(selftestKeyGap)
    }

    /// Window coordinates of the centre of guest pixel (gx, gy) in the
    /// aspect-fit frame SurfaceView draws (and maps pointer positions from).
    private func windowPoint(gx: Double, gy: Double) -> NSPoint {
        let size = surface.frameSize
        let bounds = surface.bounds
        let scale = min(bounds.width / size.width, bounds.height / size.height)
        let (w, h) = (size.width * scale, size.height * scale)
        let (ox, oy) = ((bounds.width - w) / 2, (bounds.height - h) / 2)
        let view = NSPoint(x: ox + (gx + 0.5) * scale, y: oy + h - (gy + 0.5) * scale)
        return surface.convert(view, to: nil)
    }

    private func move(gx: Double, gy: Double) {
        onMain {
            guard self.surface.frameSize.width > 0 else { fail("selftest: move before the first scanout") }
            self.pointer = self.windowPoint(gx: gx, gy: gy)
            self.postMouse(.mouseMoved)
            selftestNote(String(format: "move gx=%.0f gy=%.0f window_x=%.2f window_y=%.2f", gx, gy,
                                self.pointer.x, self.pointer.y))
        }
        usleep(selftestKeyGap)
    }

    private func click() {
        onMain {
            self.postMouse(.leftMouseDown)
            self.postMouse(.leftMouseUp)
        }
        selftestNote("click")
        usleep(selftestKeyGap)
    }

    private func postMouse(_ type: NSEvent.EventType) {
        guard let event = NSEvent.mouseEvent(
            with: type, location: pointer, modifierFlags: [],
            timestamp: ProcessInfo.processInfo.systemUptime, windowNumber: window.windowNumber,
            context: nil, eventNumber: 0, clickCount: type == .mouseMoved ? 0 : 1, pressure: 0)
        else { fail("selftest: could not build mouse event") }
        window.sendEvent(event)
    }

    private func frameInfo() {
        let f = window.frame
        let content = window.contentLayoutRect
        selftestNote(String(format: "frame-info window=%d x=%.0f y=%.0f width=%.0f height=%.0f content_width=%.0f content_height=%.0f scale=%.1f key=%d",
                            window.windowNumber, f.minX, f.minY, f.width, f.height,
                            content.width, content.height, window.backingScaleFactor, window.isKeyWindow ? 1 : 0))
    }
}
