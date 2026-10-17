// Ternvale dev viewer (Step 44): a throwaway AppKit window showing one
// virtio-gpu scanout streamed by `ternvale run --display window`. Not the
// final app (that is the Swift app of Step 55, over the control socket).
//
// Wire format: ternvale-gpu/src/present_protocol.rs (keep the two in sync).
// Frames arrive one at a time; once a frame is committed to the window server
// and the next display refresh has passed, we answer PRESENTED so the VMM can
// send the next one and log flush-to-present latency. Needs macOS 14
// (NSView.displayLink).
//
// Usage: ternvale-viewer --socket <path> [--title <text>] [--headless [--frames N]]
// --headless prints one line per frame (with an FNV-1a hash of the pixels)
// instead of opening a window; tests use it.

import AppKit
import Darwin

let wireMagic: UInt32 = 0x5044_5654  // "TVDP" read little-endian
let wireVersion: UInt16 = 1
let kindFrame: UInt16 = 0x0001
let kindHello: UInt16 = 0x0101
let kindPresented: UInt16 = 0x0102
let frameHeaderLen = 40
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
        default:
            fail("unknown argument \(arg)\nusage: ternvale-viewer --socket <path> [--title <text>] [--headless [--frames N]]")
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
}

func le16(_ b: [UInt8], _ at: Int) -> UInt16 { UInt16(b[at]) | UInt16(b[at + 1]) << 8 }
func le32(_ b: [UInt8], _ at: Int) -> UInt32 { UInt32(le16(b, at)) | UInt32(le16(b, at + 2)) << 16 }
func le64(_ b: [UInt8], _ at: Int) -> UInt64 { UInt64(le32(b, at)) | UInt64(le32(b, at + 4)) << 32 }

/// The next frame, nil when the VMM closed the socket; exits on a bad header.
func readFrame(_ link: Link) -> (FrameHeader, [UInt8])? {
    guard let raw = link.read(frameHeaderLen) else { return nil }
    guard le32(raw, 0) == wireMagic else { fail(String(format: "bad magic %#010x; not a ternvale presenter", le32(raw, 0))) }
    let version = le16(raw, 4)
    guard version == wireVersion else {
        fail("wire version \(version) is not supported (this viewer speaks \(wireVersion)); rebuild with scripts/build-viewer.sh")
    }
    guard le16(raw, 6) == kindFrame else { fail("unknown message kind \(le16(raw, 6))") }
    let h = FrameHeader(width: le32(raw, 8), height: le32(raw, 12), stride: le32(raw, 16),
                        format: le32(raw, 20), sequence: le64(raw, 24), payloadLen: le32(raw, 32))
    guard h.width <= maxDimension, h.height <= maxDimension,
          UInt64(h.stride) == UInt64(h.width) * 4,
          UInt64(h.payloadLen) == UInt64(h.stride) * UInt64(h.height)
    else { fail("frame \(h.width)x\(h.height) stride \(h.stride) does not match \(h.payloadLen) bytes") }
    if h.payloadLen == 0 { return (h, []) }
    guard let pixels = link.read(Int(h.payloadLen)) else { return nil }
    return (h, pixels)
}

/// Core Graphics layout for a `VIRTIO_GPU_FORMAT_*` (names give memory byte
/// order, virtio 1.2 §5.7.6.8). Alpha is ignored: the window is opaque.
func bitmapInfo(_ format: UInt32) -> CGBitmapInfo? {
    let little = CGBitmapInfo.byteOrder32Little.rawValue
    let big = CGBitmapInfo.byteOrder32Big.rawValue
    let skipFirst = CGImageAlphaInfo.noneSkipFirst.rawValue
    let skipLast = CGImageAlphaInfo.noneSkipLast.rawValue
    switch format {
    case 1, 2: return CGBitmapInfo(rawValue: little | skipFirst)  // B,G,R,A|X
    case 3, 4: return CGBitmapInfo(rawValue: big | skipFirst)  // A|X,R,G,B
    case 67, 134: return CGBitmapInfo(rawValue: big | skipLast)  // R,G,B,A|X
    case 68, 121: return CGBitmapInfo(rawValue: little | skipLast)  // X|A,B,G,R
    default: return nil
    }
}

func makeImage(_ h: FrameHeader, _ pixels: [UInt8]) -> CGImage? {
    guard h.width > 0, h.height > 0, let info = bitmapInfo(h.format),
          let provider = CGDataProvider(data: Data(pixels) as CFData)
    else { return nil }
    return CGImage(width: Int(h.width), height: Int(h.height), bitsPerComponent: 8,
                   bitsPerPixel: 32, bytesPerRow: Int(h.stride),
                   space: CGColorSpace(name: CGColorSpace.sRGB) ?? CGColorSpaceCreateDeviceRGB(),
                   bitmapInfo: info, provider: provider, decode: nil,
                   shouldInterpolate: false, intent: .defaultIntent)
}

func fnv1a(_ bytes: [UInt8]) -> UInt64 {
    var hash: UInt64 = 0xcbf2_9ce4_8422_2325
    for b in bytes {
        hash = (hash ^ UInt64(b)) &* 0x0000_0100_0000_01b3
    }
    return hash
}

func runHeadless(_ link: Link, frames: Int) -> Never {
    var count = 0
    while let (h, pixels) = readFrame(link) {
        print("frame sequence=\(h.sequence) width=\(h.width) height=\(h.height) format=\(h.format) bytes=\(pixels.count) fnv1a=\(String(format: "%016llx", fnv1a(pixels)))")
        fflush(stdout)
        guard link.send(kind: kindPresented, value: h.sequence) else { fail("presenter went away") }
        count += 1
        if frames > 0 && count >= frames { exit(0) }
    }
    exit(frames > 0 && count < frames ? 1 : 0)
}

final class Viewer: NSObject, NSApplicationDelegate {
    let link: Link
    let title: String
    var window: NSWindow!
    var surface: NSView!
    var shown: (UInt32, UInt32) = (0, 0)
    var refresh: CADisplayLink?
    var pendingAck: UInt64?

    init(link: Link, title: String) {
        self.link = link
        self.title = title
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
        surface = NSView()
        surface.wantsLayer = true
        surface.layer?.backgroundColor = NSColor.black.cgColor
        surface.layer?.contentsGravity = .resizeAspect
        surface.layer?.magnificationFilter = .nearest
        window.contentView = surface
        window.center()
        window.makeKeyAndOrderFront(nil)
        let refresh = surface.displayLink(target: self, selector: #selector(onRefresh(_:)))
        refresh.add(to: .main, forMode: .common)
        refresh.isPaused = true
        self.refresh = refresh
        NSApp.activate(ignoringOtherApps: true)
        Thread { self.pump() }.start()
    }

    /// First display refresh after a frame was committed: the window server has
    /// composited it by now, so this is when it counts as on screen.
    /// TODO(verify): Apple documents the callback as firing once per display
    /// frame for the view's screen, not its exact phase relative to scan-out;
    /// treat the ack as on-glass to within one refresh interval.
    @objc func onRefresh(_ sender: CADisplayLink) {
        sender.isPaused = true
        guard let sequence = pendingAck else { return }
        pendingAck = nil
        if !link.send(kind: kindPresented, value: sequence) { NSApp.terminate(nil) }
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool { true }

    /// Reader thread: at most one frame is in flight, so frames never queue up here.
    func pump() {
        while let (h, pixels) = readFrame(link) {
            let image = makeImage(h, pixels)
            if image == nil && h.width > 0 { warn("cannot draw format \(h.format); showing black") }
            DispatchQueue.main.async { self.show(h, image) }
        }
        DispatchQueue.main.async { NSApp.terminate(nil) }
    }

    func show(_ h: FrameHeader, _ image: CGImage?) {
        if (h.width, h.height) != shown {
            shown = (h.width, h.height)
            window.title = h.width > 0 ? "\(title) — \(h.width)×\(h.height)" : "\(title) — no scanout"
            if h.width > 0 {
                var size = NSSize(width: CGFloat(h.width), height: CGFloat(h.height))
                if let visible = window.screen?.visibleFrame.size {
                    let scale = min(1, visible.width / size.width, (visible.height - 40) / size.height)
                    size = NSSize(width: size.width * scale, height: size.height * scale)
                }
                window.contentAspectRatio = NSSize(width: CGFloat(h.width), height: CGFloat(h.height))
                window.setContentSize(size)
            }
        }
        CATransaction.begin()
        CATransaction.setDisableActions(true)
        surface.layer?.contents = image
        CATransaction.commit()
        // Hand the frame to the window server now rather than at the end of
        // this run-loop turn; the ack waits for the next refresh (onRefresh).
        CATransaction.flush()
        pendingAck = h.sequence
        refresh?.isPaused = false
    }
}

signal(SIGPIPE, SIG_IGN)
let options = parseOptions()
let link = Link(path: options.socket)
guard link.send(kind: kindHello, value: UInt64(wireVersion)) else { fail("could not greet the presenter") }
if options.headless { runHeadless(link, frames: options.frames) }
let app = NSApplication.shared
app.setActivationPolicy(.regular)
let viewer = Viewer(link: link, title: options.title)
app.delegate = viewer
app.run()
