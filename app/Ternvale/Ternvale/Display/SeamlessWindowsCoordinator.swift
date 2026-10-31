import AppKit
import Darwin
import Foundation
import MetalKit
import os
import SwiftUI

struct GuestWindowRecord: Codable, Hashable, Equatable {
    let id: UInt32
    let title: String
    let x: Int32
    let y: Int32
    let width: UInt32
    let height: UInt32
}

private struct SeamlessUpdate: Codable {
    let windows: [GuestWindowRecord]
    let sequence: UInt64
}

/// One native window compositing a crop of the shared guest framebuffer.
@MainActor
final class SeamlessWindowsCoordinator: ObservableObject {
    @Published private(set) var isActive = false
    @Published private(set) var windowCount = 0

    private let display: VMDisplayController
    private let log = Logger(subsystem: "com.ternvale.app", category: "seamless")
    private var readerTask: Task<Void, Never>?
    private var frameTask: Task<Void, Never>?
    private var hosts: [UInt32: SeamlessGuestWindowHost] = [:]
    private var lastRecords: [UInt32: GuestWindowRecord] = [:]

    init(display: VMDisplayController) {
        self.display = display
    }

    func start(vmName: String) {
        guard AppSettings.experimentalSeamlessWindows else {
            log.debug("seamless start skipped; toggle off")
            return
        }
        stop()
        isActive = true
        log.info("experimental seamless windows active for \(vmName, privacy: .public)")
        requestGuestDesktopSize()
        frameTask = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                guard let self else { return }
                if let frame = self.display.latestFrame {
                    for host in self.hosts.values {
                        host.submit(frame: frame)
                    }
                }
                try? await Task.sleep(nanoseconds: 16_000_000)
            }
        }
        readerTask = Task.detached(priority: .utility) { [weak self] in
            await self?.connectLoop(vmName: vmName)
        }
    }

    /// Tear down host windows and return to embedded single-window display.
    func stop() {
        readerTask?.cancel()
        readerTask = nil
        frameTask?.cancel()
        frameTask = nil
        for host in hosts.values {
            host.close()
        }
        hosts.removeAll()
        lastRecords.removeAll()
        windowCount = 0
        if isActive {
            log.info("seamless windows stopped; embedded display mode restored")
        }
        isActive = false
    }

    private func requestGuestDesktopSize() {
        guard let screen = NSScreen.main else { return }
        let scale = screen.backingScaleFactor
        let w = UInt32(max(640, (screen.frame.width * scale).rounded(.down)))
        let h = UInt32(max(480, (screen.frame.height * scale).rounded(.down)))
        display.requestHostSize(width: w, height: h)
        log.info(
            "seamless requested guest desktop \(w, privacy: .public)x\(h, privacy: .public) for framebuffer crops"
        )
    }

    private func connectLoop(vmName: String) async {
        let path = TernvalePaths.seamlessSocket(for: vmName).path
        while !Task.isCancelled {
            if FileManager.default.fileExists(atPath: path) {
                do {
                    let client = try SeamlessLineSocket(path: path)
                    log.debug("connected seamless socket \(path, privacy: .public)")
                    while !Task.isCancelled {
                        guard let line = client.readLine() else { break }
                        guard let data = line.data(using: .utf8) else { continue }
                        let update = try JSONDecoder().decode(SeamlessUpdate.self, from: data)
                        await apply(update: update)
                    }
                    log.debug("seamless socket read ended; reconnecting")
                } catch {
                    log.debug("seamless connect failed: \(error.localizedDescription, privacy: .public)")
                }
            }
            try? await Task.sleep(nanoseconds: 300_000_000)
        }
    }

    private func apply(update: SeamlessUpdate) {
        let incomingSet = Set(update.windows.map(\.id))
        let currentSet = Set(hosts.keys)
        for id in SeamlessWindowSync.removedIds(current: currentSet, incoming: incomingSet) {
            hosts[id]?.close()
            hosts.removeValue(forKey: id)
            lastRecords.removeValue(forKey: id)
            log.info("closed seamless host window for guest id=\(id, privacy: .public) (guest window gone)")
        }
        for record in update.windows {
            if let host = hosts[record.id] {
                if lastRecords[record.id] != record {
                    log.debug(
                        "moved seamless window id=\(record.id, privacy: .public) to \(record.x),\(record.y) \(record.width)x\(record.height)"
                    )
                    host.update(record: record, guestSize: display.guestPixelSize)
                    lastRecords[record.id] = record
                }
            } else {
                let host = SeamlessGuestWindowHost(record: record, guestSize: display.guestPixelSize)
                hosts[record.id] = host
                lastRecords[record.id] = record
                log.info(
                    "opened seamless window id=\(record.id, privacy: .public) title=\(record.title, privacy: .public) frame=\(record.width)x\(record.height)"
                )
                if let frame = display.latestFrame {
                    host.submit(frame: frame)
                }
            }
        }
        windowCount = hosts.count
        if update.windows.count >= 2 {
            log.debug("seamless tracking \(update.windows.count, privacy: .public) guest windows")
        }
    }
}

@MainActor
private final class SeamlessGuestWindowHost {
    let window: NSWindow
    private let metalView: SeamlessCropMetalView

    init(record: GuestWindowRecord, guestSize: CGSize) {
        metalView = SeamlessCropMetalView(crop: Self.cropRect(for: record))
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: Int(record.width), height: Int(record.height)),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = record.title.isEmpty ? "Guest \(record.id)" : record.title
        window.contentView = metalView
        window.isReleasedWhenClosed = false
        window.collectionBehavior = [.moveToActiveSpace]
        position(record: record, guestSize: guestSize)
        window.makeKeyAndOrderFront(nil)
    }

    func update(record: GuestWindowRecord, guestSize: CGSize) {
        window.title = record.title.isEmpty ? "Guest \(record.id)" : record.title
        let size = NSSize(width: Int(record.width), height: Int(record.height))
        metalView.frame = NSRect(origin: .zero, size: size)
        metalView.cropRect = Self.cropRect(for: record)
        position(record: record, guestSize: guestSize)
    }

    func submit(frame: GuestDisplayFrame) {
        metalView.submit(frame: frame)
    }

    func close() {
        window.orderOut(nil)
        window.close()
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
        let frame = NSRect(
            x: screenFrame.origin.x + x,
            y: screenFrame.origin.y + screenFrame.height - y - h,
            width: w,
            height: h
        )
        window.setFrame(frame, display: true)
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

private final class SeamlessCropMetalView: MTKView, MTKViewDelegate {
    var cropRect: CGRect = .zero {
        didSet { setNeedsDisplay(true) }
    }

    private var queue: MTLCommandQueue?
    private var texture: MTLTexture?

    init(crop: CGRect) {
        let device = MTLCreateSystemDefaultDevice()
        super.init(frame: NSRect(origin: .zero, size: crop.size), device: device)
        cropRect = crop
        queue = device?.makeCommandQueue()
        framebufferOnly = false
        colorPixelFormat = .bgra8Unorm
        isPaused = true
        enableSetNeedsDisplay = false
        delegate = self
        wantsLayer = true
        layer?.backgroundColor = NSColor.black.cgColor
        autoresizingMask = [.width, .height]
        contentScaleFactor = 1
    }

    @available(*, unavailable)
    required init(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func submit(frame: GuestDisplayFrame) {
        guard frame.width > 0, frame.height > 0, !frame.pixels.isEmpty else { return }
        guard let device else { return }
        let desc = MTLTextureDescriptor.texture2DDescriptor(
            pixelFormat: metalFormat(frame.format),
            width: Int(frame.width),
            height: Int(frame.height),
            mipmapped: false
        )
        desc.usage = [.shaderRead]
        guard let tex = device.makeTexture(descriptor: desc) else { return }
        let stride = Int(frame.width) * 4
        frame.pixels.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            tex.replace(
                region: MTLRegionMake2D(0, 0, Int(frame.width), Int(frame.height)),
                mipmapLevel: 0,
                withBytes: base,
                bytesPerRow: stride
            )
        }
        texture = tex
        isPaused = false
        draw()
    }

    func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}

    func draw(in view: MTKView) {
        guard let drawable = currentDrawable,
              let queue,
              let texture,
              let commandBuffer = queue.makeCommandBuffer(),
              let blit = commandBuffer.makeBlitCommandEncoder()
        else { return }

        let ox = max(0, min(Int(cropRect.origin.x), texture.width - 1))
        let oy = max(0, min(Int(cropRect.origin.y), texture.height - 1))
        let cw = max(1, min(Int(cropRect.width), texture.width - ox))
        let ch = max(1, min(Int(cropRect.height), texture.height - oy))

        if let clear = commandBuffer.makeRenderCommandEncoder(
            descriptor: clearPassDescriptor(texture: drawable.texture)
        ) {
            clear.endEncoding()
        }

        blit.copy(
            from: texture,
            sourceSlice: 0,
            sourceLevel: 0,
            sourceOrigin: MTLOrigin(x: ox, y: oy, z: 0),
            sourceSize: MTLSize(width: cw, height: ch, depth: 1),
            to: drawable.texture,
            destinationSlice: 0,
            destinationLevel: 0,
            destinationOrigin: MTLOrigin(x: 0, y: 0, z: 0)
        )
        blit.endEncoding()
        commandBuffer.present(drawable)
        commandBuffer.commit()
        isPaused = true
    }

    private func clearPassDescriptor(texture: MTLTexture) -> MTLRenderPassDescriptor {
        let pass = MTLRenderPassDescriptor()
        pass.colorAttachments[0].texture = texture
        pass.colorAttachments[0].loadAction = .clear
        pass.colorAttachments[0].storeAction = .store
        pass.colorAttachments[0].clearColor = MTLClearColor(red: 0, green: 0, blue: 0, alpha: 1)
        return pass
    }

    private func metalFormat(_ virtio: UInt32) -> MTLPixelFormat {
        switch virtio {
        case 1, 2, 68, 121: return .bgra8Unorm
        case 3, 4, 67, 134: return .rgba8Unorm
        default: return .bgra8Unorm
        }
    }
}

private final class SeamlessLineSocket {
    private let fd: Int32

    init(path: String) throws {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        var addr = sockaddr_un()
        let bytes = Array(path.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else {
            close(fd)
            throw NSError(domain: "SeamlessLineSocket", code: 1)
        }
        addr.sun_family = sa_family_t(AF_UNIX)
        addr.sun_len = UInt8(MemoryLayout<sockaddr_un>.size)
        withUnsafeMutableBytes(of: &addr.sun_path) { $0.copyBytes(from: bytes) }
        let rc = withUnsafePointer(to: &addr) {
            $0.withMemoryRebound(to: sockaddr.self, capacity: 1) {
                connect(fd, $0, socklen_t(MemoryLayout<sockaddr_un>.size))
            }
        }
        guard rc == 0 else {
            close(fd)
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        self.fd = fd
    }

    deinit {
        close(fd)
    }

    func readLine() -> String? {
        var bytes: [UInt8] = []
        while true {
            var ch: UInt8 = 0
            let n = Darwin.read(fd, &ch, 1)
            if n == 1 {
                if ch == 0x0A { break }
                bytes.append(ch)
            } else if n < 0 && errno == EINTR {
                continue
            } else {
                return bytes.isEmpty ? nil : String(bytes: bytes, encoding: .utf8)
            }
        }
        return String(bytes: bytes, encoding: .utf8)
    }
}
