// CAMetalLayer presentation (Step 109). Minimum macOS is 14.0
// (`scripts/build-viewer.sh` `-target arm64-apple-macos14.0`).
// CAMetalDisplayLink (macOS 14) paces presents to the display; CVDisplayLink
// is not used. Guest pixels are sRGB; Display P3 only sets the layer color space.

import AppKit
import Metal
import QuartzCore

final class MetalScanout: NSObject {
    let layer = CAMetalLayer()
    private let device: MTLDevice
    private let queue: MTLCommandQueue
    private var texture: MTLTexture?
    private var size = (w: 0, h: 0)
    private var pendingAck: UInt64?
    private var displayLink: CAMetalDisplayLink?
    weak var owner: Viewer?

    convenience init?(metal _: ()) {
        guard MTLCreateSystemDefaultDevice() != nil else { return nil }
        self.init()
    }

    override init() {
        let device = MTLCreateSystemDefaultDevice()!
        let queue = device.makeCommandQueue()!
        self.device = device
        self.queue = queue
        super.init()
        layer.device = device
        layer.pixelFormat = .bgra8Unorm_srgb
        // Blit into the drawable; framebufferOnly would reject the copy.
        layer.framebufferOnly = false
        layer.isOpaque = true
        // displaySyncEnabled — macOS 10.13; vsync with the drawable.
        layer.displaySyncEnabled = true
        layer.colorspace = CGColorSpace(name: CGColorSpace.sRGB)
    }

    func attach(to view: NSView, p3: Bool) {
        view.wantsLayer = true
        view.layer = layer
        applyColor(p3: p3)
        // CAMetalDisplayLink — macOS 14.0.
        let link = CAMetalDisplayLink(metalLayer: layer)
        link.delegate = self
        link.add(to: .main, forMode: .common)
        link.isPaused = true
        displayLink = link
    }

    func applyColor(p3: Bool) {
        if p3 {
            layer.colorspace = CGColorSpace(name: CGColorSpace.displayP3)
        } else {
            layer.colorspace = CGColorSpace(name: CGColorSpace.sRGB)
        }
    }

    func submit(header: FrameHeader, pixels: [UInt8]) {
        let w = Int(header.width)
        let h = Int(header.height)
        guard w > 0, h > 0 else { return }
        if size != (w, h) {
            let desc = MTLTextureDescriptor.texture2DDescriptor(
                pixelFormat: .bgra8Unorm_srgb, width: w, height: h, mipmapped: false
            )
            desc.usage = [.shaderRead]
            texture = device.makeTexture(descriptor: desc)
            size = (w, h)
        }
        guard let texture else { return }
        let partial = header.flags & 1 != 0
        let dx = Int(partial ? header.damageX : 0)
        let dy = Int(partial ? header.damageY : 0)
        let dw = Int(partial ? header.damageW : header.width)
        let dh = Int(partial ? header.damageH : header.height)
        guard dw > 0, dh > 0, !pixels.isEmpty else {
            pendingAck = header.sequence
            displayLink?.isPaused = false
            return
        }
        pixels.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            texture.replace(
                region: MTLRegionMake2D(dx, dy, dw, dh),
                mipmapLevel: 0,
                withBytes: base,
                bytesPerRow: dw * 4
            )
        }
        pendingAck = header.sequence
        displayLink?.isPaused = false
    }
}

extension MetalScanout: CAMetalDisplayLinkDelegate {
    // CAMetalDisplayLinkDelegate.metalDisplayLink(_:needsUpdate:) — macOS 14.0.
    func metalDisplayLink(_ link: CAMetalDisplayLink, needsUpdate update: CAMetalDisplayLink.Update) {
        link.isPaused = true
        let drawable = update.drawable
        guard let texture,
              let commandBuffer = queue.makeCommandBuffer(),
              let blit = commandBuffer.makeBlitCommandEncoder()
        else {
            ack()
            return
        }
        let dst = drawable.texture
        let sw = min(texture.width, dst.width)
        let sh = min(texture.height, dst.height)
        blit.copy(
            from: texture,
            sourceSlice: 0,
            sourceLevel: 0,
            sourceOrigin: MTLOrigin(x: 0, y: 0, z: 0),
            sourceSize: MTLSize(width: sw, height: sh, depth: 1),
            to: dst,
            destinationSlice: 0,
            destinationLevel: 0,
            destinationOrigin: MTLOrigin(x: 0, y: 0, z: 0)
        )
        blit.endEncoding()
        commandBuffer.present(drawable)
        commandBuffer.addCompletedHandler { [weak self] _ in
            DispatchQueue.main.async { self?.ack() }
        }
        commandBuffer.commit()
    }

    private func ack() {
        guard let sequence = pendingAck else { return }
        pendingAck = nil
        owner?.ackPresented(sequence)
    }
}
