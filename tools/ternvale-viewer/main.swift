// Ternvale dev viewer (Step 44): a throwaway AppKit window showing one
// virtio-gpu scanout streamed by `ternvale run --display window`. Not the
// final app (that is the Swift app of Step 55, over the control socket).
//
// Wire format: ternvale-gpu/src/present_protocol.rs (keep the two in sync).
// Frames arrive one at a time; once a frame is committed to the window server
// and the next display refresh has passed, we answer PRESENTED so the VMM can
// send the next one and log flush-to-present latency. Keyboard and mouse
// input in the window goes back to the VMM (input.swift). Needs macOS 14
// (NSView.displayLink).
//
// Usage: ternvale-viewer --socket <path> [--title <text>]
//                        [--headless [--frames N] [--script-input]]
// --headless prints one line per frame (with an FNV-1a hash of the pixels)
// instead of opening a window; --script-input then sends a fixed input
// sequence after the first frame. Tests use both. TERNVALE_VIEWER_SELFTEST
// drives the real window from a script (selftest.swift).

import AppKit
import Darwin

let wireMagic: UInt32 = 0x5044_5654  // "TVDP" read little-endian
let wireVersion: UInt16 = 4
let kindFrame: UInt16 = 0x0001
let kindCursor: UInt16 = 0x0002
let kindCaptureHint: UInt16 = 0x0003
let kindHello: UInt16 = 0x0101
let kindPresented: UInt16 = 0x0102
let kindSetSizeEx: UInt16 = 0x0206
let frameHeaderLen = 64
let frameFlagPartial: UInt32 = 1
let frameFlagDisplayP3: UInt32 = 2
let maxDimension: UInt32 = 16_384

func warn(_ message: String) {
    FileHandle.standardError.write(Data("ternvale-viewer: \(message)\n".utf8))
}

func fail(_ message: String) -> Never {
    warn(message)
    exit(1)
}

struct Options {
    var socket = ""
    var title = "Ternvale"
    var headless = false
    var frames = 0
    var scriptInput = false
    var perScanout = false
}

func parseOptions() -> Options {
    var options = Options()
    var args = CommandLine.arguments.dropFirst().makeIterator()
    func value(_ flag: String) -> String {
        guard let v = args.next() else { fail("\(flag) needs a value") }
        return v
    }
    while let arg = args.next() {
        switch arg {
        case "--socket": options.socket = value(arg)
        case "--title": options.title = value(arg)
        case "--headless": options.headless = true
        case "--frames":
            guard let n = Int(value(arg)), n >= 0 else { fail("--frames needs a count") }
            options.frames = n
        case "--script-input": options.scriptInput = true
        case "--per-scanout": options.perScanout = true
        default:
            fail("unknown argument \(arg)\nusage: ternvale-viewer --socket <path> [--title <text>] [--headless [--frames N] [--script-input]] [--per-scanout]")
        }
    }
    if options.socket.isEmpty { fail("--socket <path> is required") }
    return options
}

/// The Unix stream socket to the VMM's presenter.
final class Link {
    let fd: Int32
    private let writeLock = NSLock()

    init(path: String) {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else { fail("socket: \(String(cString: strerror(errno)))") }
        var addr = sockaddr_un()
        let bytes = Array(path.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else {
            fail("socket path too long: \(path)")
        }
        addr.sun_family = sa_family_t(AF_UNIX)
        addr.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        withUnsafeMutableBytes(of: &addr.sun_path) { $0.copyBytes(from: bytes) }
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard rc == 0 else { fail("connect \(path): \(String(cString: strerror(errno)))") }
        self.fd = fd
    }

    /// Exactly `count` bytes, or nil at EOF / error.
    func read(_ count: Int) -> [UInt8]? {
        var buf = [UInt8](repeating: 0, count: count)
        var got = 0
        while got < count {
            let n = buf.withUnsafeMutableBytes { Darwin.read(fd, $0.baseAddress! + got, count - got) }
            if n > 0 {
                got += n
            } else if n < 0 && errno == EINTR {
                continue
            } else {
                return nil
            }
        }
        return buf
    }

    /// One 16-byte viewer → VMM message; false if the VMM is gone.
    func send(kind: UInt16, value: UInt64) -> Bool {
        var msg = [UInt8]()
        msg += withUnsafeBytes(of: wireMagic.littleEndian, Array.init)
        msg += withUnsafeBytes(of: wireVersion.littleEndian, Array.init)
        msg += withUnsafeBytes(of: kind.littleEndian, Array.init)
        msg += withUnsafeBytes(of: value.littleEndian, Array.init)
        writeLock.lock()
        defer { writeLock.unlock() }
        var sent = 0
        while sent < msg.count {
            let n = msg.withUnsafeBytes { Darwin.write(fd, $0.baseAddress! + sent, msg.count - sent) }
            if n > 0 {
                sent += n
            } else if n < 0 && errno == EINTR {
                continue
            } else {
                return false
            }
        }
        return true
    }
}

struct FrameHeader {
    var width: UInt32
    var height: UInt32
    var stride: UInt32
    var format: UInt32
    var sequence: UInt64
    var payloadLen: UInt32
    var flags: UInt32
    var scanout: UInt32
    var damageX: UInt32
    var damageY: UInt32
    var damageW: UInt32
    var damageH: UInt32
    var scaleX100: UInt32
}

func le16(_ b: [UInt8], _ at: Int) -> UInt16 { UInt16(b[at]) | UInt16(b[at + 1]) << 8 }
func le32(_ b: [UInt8], _ at: Int) -> UInt32 { UInt32(le16(b, at)) | UInt32(le16(b, at + 2)) << 16 }
func le64(_ b: [UInt8], _ at: Int) -> UInt64 { UInt64(le32(b, at)) | UInt64(le32(b, at + 4)) << 32 }

enum DisplayMsg {
    case frame(FrameHeader, [UInt8])
    case cursor(FrameHeader, [UInt8])
    case captureHint(Bool)
}

/// The next VMM → viewer header, nil when the VMM closed the socket.
func readDisplay(_ link: Link) -> DisplayMsg? {
    guard let raw = link.read(frameHeaderLen) else { return nil }
    guard le32(raw, 0) == wireMagic else { fail(String(format: "bad magic %#010x; not a ternvale presenter", le32(raw, 0))) }
    let version = le16(raw, 4)
    guard version == wireVersion else {
        fail("wire version \(version) is not supported (this viewer speaks \(wireVersion)); rebuild with scripts/build-viewer.sh")
    }
    let kind = le16(raw, 6)
    let h = FrameHeader(
        width: le32(raw, 8), height: le32(raw, 12), stride: le32(raw, 16),
        format: le32(raw, 20), sequence: le64(raw, 24), payloadLen: le32(raw, 32),
        flags: le32(raw, 36), scanout: le32(raw, 40),
        damageX: le32(raw, 44), damageY: le32(raw, 48),
        damageW: le32(raw, 52), damageH: le32(raw, 56), scaleX100: le32(raw, 60)
    )
    if kind == kindCaptureHint {
        return .captureHint(h.flags & 1 != 0)
    }
    let expected: UInt64 = {
        if h.width == 0 && h.height == 0 { return 0 }
        if kind == kindCursor { return UInt64(h.width) * 4 * UInt64(h.height) }
        if h.flags & frameFlagPartial != 0 { return UInt64(h.damageW) * 4 * UInt64(h.damageH) }
        return UInt64(h.stride) * UInt64(h.height)
    }()
    guard h.width <= maxDimension, h.height <= maxDimension,
          UInt64(h.stride) == UInt64(h.width) * 4,
          UInt64(h.payloadLen) == expected
    else { fail("frame \(h.width)x\(h.height) stride \(h.stride) does not match \(h.payloadLen) bytes") }
    let pixels: [UInt8]
    if h.payloadLen == 0 {
        pixels = []
    } else {
        guard let body = link.read(Int(h.payloadLen)) else { return nil }
        pixels = body
    }
    if kind == kindFrame { return .frame(h, pixels) }
    if kind == kindCursor { return .cursor(h, pixels) }
    fail("unknown message kind \(kind)")
}

func readFrame(_ link: Link) -> (FrameHeader, [UInt8])? {
    while let msg = readDisplay(link) {
        if case let .frame(h, pixels) = msg { return (h, pixels) }
    }
    return nil
}

func fnv1a(_ bytes: [UInt8]) -> UInt64 {
    var hash: UInt64 = 0xcbf2_9ce4_8422_2325
    for b in bytes {
        hash = (hash ^ UInt64(b)) &* 0x0000_0100_0000_01b3
    }
    return hash
}

func runHeadless(_ link: Link, frames: Int, scriptInput: Bool) -> Never {
    var count = 0
    while let (h, pixels) = readFrame(link) {
        print("frame sequence=\(h.sequence) width=\(h.width) height=\(h.height) format=\(h.format) bytes=\(pixels.count) fnv1a=\(String(format: "%016llx", fnv1a(pixels)))")
        fflush(stdout)
        guard link.send(kind: kindPresented, value: h.sequence) else { fail("presenter went away") }
        count += 1
        if scriptInput && count == 1 { sendScriptedInput(InputSender(link: link)) }
        if frames > 0 && count >= frames { exit(0) }
    }
    exit(frames > 0 && count < frames ? 1 : 0)
}

final class Viewer: NSObject, NSApplicationDelegate, NSWindowDelegate {
    let link: Link
    let title: String
    let perScanout: Bool
    var window: NSWindow!
    var surface: SurfaceView!
    var metal: MetalScanout?
    var extras: [UInt32: (NSWindow, SurfaceView, MetalScanout)] = [:]
    var shown: (UInt32, UInt32) = (0, 0)

    init(link: Link, title: String, perScanout: Bool) {
        self.link = link
        self.title = title
        self.perScanout = perScanout
    }

    func ackPresented(_ sequence: UInt64) {
        if !link.send(kind: kindPresented, value: sequence) { NSApp.terminate(nil) }
    }

    func applicationDidFinishLaunching(_ notification: Notification) {
        let menu = NSMenu()
        let appItem = NSMenuItem()
        appItem.submenu = NSMenu()
        appItem.submenu?.addItem(withTitle: "Quit \(title)", action: #selector(NSApplication.terminate(_:)), keyEquivalent: "q")
        menu.addItem(appItem)
        NSApp.mainMenu = menu

        window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 1280, height: 800),
                          styleMask: [.titled, .closable, .miniaturizable, .resizable],
                          backing: .buffered, defer: false)
        window.title = "\(title) — waiting for scanout"
        window.collectionBehavior = [.fullScreenPrimary, .managed]
        surface = SurfaceView()
        surface.input = InputSender(link: link)
        GamepadWatcher.shared.attach(input: surface.input!)
        window.contentView = surface
        if let metal = MetalScanout(metal: ()) {
            metal.owner = self
            metal.attach(to: surface, p3: false)
            self.metal = metal
        }
        window.delegate = self
        window.acceptsMouseMovedEvents = true
        window.makeFirstResponder(surface)
        window.center()
        let viewMenu = NSMenu(title: "View")
        viewMenu.addItem(withTitle: "Enter Full Screen", action: #selector(NSWindow.toggleFullScreen(_:)), keyEquivalent: "f")
        let viewItem = NSMenuItem()
        viewItem.submenu = viewMenu
        NSApp.mainMenu?.addItem(viewItem)
        if SelfTest.requested {
            // Don't take focus: whatever the person at the keyboard types would
            // land in the guest and garble the scripted input.
            window.orderFrontRegardless()
        } else {
            window.makeKeyAndOrderFront(nil)
        }
        if !SelfTest.requested { NSApp.activate(ignoringOtherApps: true) }
        Thread { self.pump() }.start()
        SelfTest.startIfRequested(window: window, surface: surface)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    /// Keys and buttons held when focus leaves would never see their release.
    func windowDidResignKey(_ notification: Notification) { surface.input?.releaseAll() }

    /// Reader thread: at most one frame is in flight, so frames never queue up here.
    func pump() {
        while let msg = readDisplay(link) {
            DispatchQueue.main.async {
                switch msg {
                case let .frame(h, pixels): self.show(h, pixels)
                case let .cursor(h, pixels): self.surface.applyCursor(h, pixels)
                case let .captureHint(on): self.surface.setCaptured(on, fromHint: true)
                }
            }
        }
        DispatchQueue.main.async { NSApp.terminate(nil) }
    }

    func show(_ h: FrameHeader, _ pixels: [UInt8]) {
        guard let target = window(for: h.scanout) else {
            ackPresented(h.sequence)
            return
        }
        let scale = max(1, Int(h.scaleX100 == 0 ? 100 : h.scaleX100))
        if (h.width, h.height) != shown && h.scanout == 0 {
            shown = (h.width, h.height)
            target.surface.frameSize = CGSize(width: CGFloat(h.width), height: CGFloat(h.height))
            target.window.title = h.width > 0 ? "\(title) — \(h.width)×\(h.height)" : "\(title) — no scanout"
            if h.width > 0 {
                let points = CGFloat(scale) / 100
                var size = NSSize(width: CGFloat(h.width) / points, height: CGFloat(h.height) / points)
                if let visible = target.window.screen?.visibleFrame.size {
                    let fit = min(1, visible.width / size.width, (visible.height - 40) / size.height)
                    size = NSSize(width: size.width * fit, height: size.height * fit)
                }
                target.window.contentAspectRatio = size
                target.window.setContentSize(size)
            }
        }
        target.metal.applyColor(p3: h.flags & frameFlagDisplayP3 != 0)
        target.metal.submit(header: h, pixels: pixels)
    }

    func window(for scanout: UInt32) -> (window: NSWindow, surface: SurfaceView, metal: MetalScanout)? {
        if scanout == 0, let metal {
            return (window, surface, metal)
        }
        if let extra = extras[scanout] { return extra }
        guard perScanout, let metal = MetalScanout(metal: ()) else {
            if let metal { return (window, surface, metal) }
            return nil
        }
        let extra = NSWindow(
            contentRect: NSRect(x: 40, y: 40, width: 1280, height: 800),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        extra.title = "\(title) — scanout \(scanout)"
        extra.collectionBehavior = [.fullScreenPrimary, .managed]
        let view = SurfaceView()
        extra.contentView = view
        metal.owner = self
        metal.attach(to: view, p3: false)
        extra.makeKeyAndOrderFront(nil)
        extras[scanout] = (extra, view, metal)
        return (extra, view, metal)
    }
}

signal(SIGPIPE, SIG_IGN)
let options = parseOptions()
let link = Link(path: options.socket)
guard link.send(kind: kindHello, value: UInt64(wireVersion)) else { fail("could not greet the presenter") }
if options.headless { runHeadless(link, frames: options.frames, scriptInput: options.scriptInput) }
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let viewer = Viewer(link: link, title: options.title, perScanout: options.perScanout)
app.delegate = viewer
app.run()
