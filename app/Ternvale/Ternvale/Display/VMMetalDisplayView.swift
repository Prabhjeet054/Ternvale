import MetalKit
import SwiftUI

/// Metal blit of the guest framebuffer with pointer/keyboard capture.
struct VMMetalDisplayView: NSViewRepresentable {
    @ObservedObject var controller: VMDisplayController

    func makeNSView(context: Context) -> DisplayMetalView {
        let view = DisplayMetalView()
        view.controller = controller
        return view
    }

    func updateNSView(_ nsView: DisplayMetalView, context: Context) {
        nsView.controller = controller
        if let frame = controller.latestFrame {
            nsView.submit(frame: frame)
        }
    }
}

final class DisplayMetalView: MTKView, MTKViewDelegate {
    weak var controller: VMDisplayController?
    private var deviceMetal: MTLDevice?
    private var queue: MTLCommandQueue?
    private var texture: MTLTexture?
    private var frameSize = CGSize.zero
    private var tracking: NSTrackingArea?

    override init(frame frameRect: NSRect, device: MTLDevice?) {
        let dev = device ?? MTLCreateSystemDefaultDevice()
        super.init(frame: frameRect, device: dev)
        deviceMetal = dev
        queue = dev?.makeCommandQueue()
        framebufferOnly = false
        colorPixelFormat = .bgra8Unorm
        isPaused = true
        enableSetNeedsDisplay = false
        delegate = self
        wantsLayer = true
        layer?.backgroundColor = NSColor.black.cgColor
    }

    @available(*, unavailable)
    required init(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    override var acceptsFirstResponder: Bool { true }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let old = tracking { removeTrackingArea(old) }
        let area = NSTrackingArea(
            rect: .zero,
            options: [.mouseMoved, .activeInKeyWindow, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        tracking = area
    }

    func submit(frame: GuestDisplayFrame) {
        guard frame.width > 0, frame.height > 0, !frame.pixels.isEmpty else { return }
        guard let deviceMetal else { return }
        let desc = MTLTextureDescriptor.texture2DDescriptor(
            pixelFormat: metalFormat(frame.format),
            width: Int(frame.width),
            height: Int(frame.height),
            mipmapped: false
        )
        desc.usage = [.shaderRead]
        guard let tex = deviceMetal.makeTexture(descriptor: desc) else { return }
        let stride = Int(frame.strideBytes)
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
        frameSize = CGSize(width: CGFloat(frame.width), height: CGFloat(frame.height))
        pendingSequence = frame.sequence
        isPaused = false
        draw()
    }

    private var pendingSequence: UInt64?

    func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}

    func draw(in view: MTKView) {
        guard let drawable = currentDrawable,
              let queue,
              let texture,
              let commandBuffer = queue.makeCommandBuffer(),
              let blit = commandBuffer.makeBlitCommandEncoder()
        else { return }

        let drawableTex = drawable.texture
        let srcW = texture.width
        let srcH = texture.height
        let dstW = drawableTex.width
        let dstH = drawableTex.height
        let scale = min(Double(dstW) / Double(srcW), Double(dstH) / Double(srcH))
        let w = max(1, Int(Double(srcW) * scale))
        let h = max(1, Int(Double(srcH) * scale))
        let ox = (dstW - w) / 2
        let oy = (dstH - h) / 2

        if let clear = commandBuffer.makeRenderCommandEncoder(
            descriptor: clearPassDescriptor(texture: drawableTex)
        ) {
            clear.endEncoding()
        }

        blit.copy(
            from: texture,
            sourceSlice: 0,
            sourceLevel: 0,
            sourceOrigin: MTLOrigin(x: 0, y: 0, z: 0),
            sourceSize: MTLSize(width: srcW, height: srcH, depth: 1),
            to: drawableTex,
            destinationSlice: 0,
            destinationLevel: 0,
            destinationOrigin: MTLOrigin(x: ox, y: oy, z: 0)
        )
        blit.endEncoding()
        commandBuffer.present(drawable)
        commandBuffer.addCompletedHandler { [weak self] _ in
            guard let self, let sequence = self.pendingSequence else { return }
            self.pendingSequence = nil
            self.controller?.acknowledgePresented(sequence: sequence)
        }
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
        case 1, 2, 68, 121:
            return .bgra8Unorm
        case 3, 4, 67, 134:
            return .rgba8Unorm
        default:
            return .bgra8Unorm
        }
    }

    private func pointerNormalized(_ event: NSEvent) -> (Double, Double)? {
        guard frameSize.width > 0, frameSize.height > 0 else { return nil }
        let p = convert(event.locationInWindow, from: nil)
        let scale = min(bounds.width / frameSize.width, bounds.height / frameSize.height)
        let w = frameSize.width * scale
        let h = frameSize.height * scale
        let ox = (bounds.width - w) / 2
        let oy = (bounds.height - h) / 2
        let x = Double((p.x - ox) / w)
        let y = Double(1 - (p.y - oy) / h)
        return (x, y)
    }

    private func movePointer(_ event: NSEvent) {
        guard let (x, y) = pointerNormalized(event) else { return }
        controller?.input.pointer(x: x, y: y)
    }

    override func keyDown(with event: NSEvent) { controller?.input.handleKeyDown(event) }
    override func keyUp(with event: NSEvent) { controller?.input.handleKeyUp(event) }
    override func flagsChanged(with event: NSEvent) { controller?.input.handleFlagsChanged(event) }
    override func mouseMoved(with event: NSEvent) { movePointer(event) }
    override func mouseDragged(with event: NSEvent) { movePointer(event) }
    override func rightMouseDragged(with event: NSEvent) { movePointer(event) }
    override func otherMouseDragged(with event: NSEvent) { movePointer(event) }
    override func mouseDown(with event: NSEvent) {
        movePointer(event)
        controller?.input.sendButton(0, pressed: true)
    }
    override func mouseUp(with event: NSEvent) { controller?.input.sendButton(0, pressed: false) }
    override func rightMouseDown(with event: NSEvent) {
        movePointer(event)
        controller?.input.sendButton(1, pressed: true)
    }
    override func rightMouseUp(with event: NSEvent) { controller?.input.sendButton(1, pressed: false) }
    override func otherMouseDown(with event: NSEvent) {
        if event.buttonNumber == 2 {
            movePointer(event)
            controller?.input.sendButton(2, pressed: true)
        }
    }
    override func otherMouseUp(with event: NSEvent) {
        if event.buttonNumber == 2 { controller?.input.sendButton(2, pressed: false) }
    }
    override func scrollWheel(with event: NSEvent) { controller?.input.handleScroll(event) }
    override func resignFirstResponder() -> Bool {
        let ok = super.resignFirstResponder()
        controller?.input.releaseAll()
        return ok
    }
}

private extension GuestDisplayFrame {
    var strideBytes: UInt32 { width * 4 }
}
