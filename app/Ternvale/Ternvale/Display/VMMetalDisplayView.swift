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
        nsView.applyCursor(controller.cursorOverlay)
        if controller.wantCapture {
            nsView.setCaptured(true, fromHint: true)
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
    private var captured = false
    private var guestCursor: NSCursor?

    override init(frame frameRect: NSRect, device: MTLDevice?) {
        let dev = device ?? MTLCreateSystemDefaultDevice()
        super.init(frame: frameRect, device: dev)
        deviceMetal = dev
        queue = dev?.makeCommandQueue()
        framebufferOnly = false
        colorPixelFormat = .bgra8Unorm_srgb
        isPaused = true
        enableSetNeedsDisplay = false
        delegate = self
        wantsLayer = true
        if let metal = layer as? CAMetalLayer {
            metal.displaySyncEnabled = true
            metal.colorspace = CGColorSpace(name: CGColorSpace.sRGB)
            metal.framebufferOnly = true
        }
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
        let w = Int(frame.width)
        let h = Int(frame.height)
        if texture == nil || texture?.width != w || texture?.height != h {
            let desc = MTLTextureDescriptor.texture2DDescriptor(
                pixelFormat: metalFormat(frame.format),
                width: w,
                height: h,
                mipmapped: false
            )
            desc.usage = [.shaderRead]
            texture = deviceMetal.makeTexture(descriptor: desc)
        }
        guard let tex = texture else { return }
        let partial = frame.flags & PresentWire.flagPartial != 0
        let dx = Int(partial ? frame.damageX : 0)
        let dy = Int(partial ? frame.damageY : 0)
        let dw = Int(partial ? frame.damageW : frame.width)
        let dh = Int(partial ? frame.damageH : frame.height)
        frame.pixels.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            tex.replace(
                region: MTLRegionMake2D(dx, dy, dw, dh),
                mipmapLevel: 0,
                withBytes: base,
                bytesPerRow: dw * 4
            )
        }
        if let metal = layer as? CAMetalLayer {
            metal.colorspace = frame.flags & PresentWire.flagDisplayP3 != 0
                ? CGColorSpace(name: CGColorSpace.displayP3)
                : CGColorSpace(name: CGColorSpace.sRGB)
        }
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

    func setCaptured(_ on: Bool, fromHint: Bool) {
        guard captured != on else { return }
        captured = on
        if !fromHint { controller?.input.capture(on) }
        if on { NSCursor.hide() } else {
            NSCursor.unhide()
            guestCursor?.set()
        }
    }

    func applyCursor(_ overlay: GuestCursorOverlay?) {
        guard let overlay, overlay.width > 0, overlay.height > 0 else {
            guestCursor = nil
            if !captured { NSCursor.arrow.set() }
            return
        }
        let pixels = [UInt8](overlay.pixels)
        guestCursor = nsCursorApp(
            width: Int(overlay.width),
            height: Int(overlay.height),
            hotX: Int(overlay.hotX),
            hotY: Int(overlay.hotY),
            pixels: pixels
        )
        if !captured { guestCursor?.set() }
    }

    private func movePointer(_ event: NSEvent) {
        if captured {
            let dx = Int32(event.deltaX.rounded())
            let dy = Int32(event.deltaY.rounded())
            if dx != 0 || dy != 0 { controller?.input.rel(dx: dx, dy: dy) }
            return
        }
        guard let (x, y) = pointerNormalized(event) else { return }
        controller?.input.pointer(x: x, y: y)
        guestCursor?.set()
    }

    override func keyDown(with event: NSEvent) {
        if event.keyCode == 0x05,
           event.modifierFlags.contains(.control),
           event.modifierFlags.contains(.option) {
            setCaptured(!captured, fromHint: false)
            return
        }
        if captured && event.keyCode == 0x35 {
            setCaptured(false, fromHint: false)
            return
        }
        controller?.input.handleKeyDown(event)
    }
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

func nsCursorApp(width: Int, height: Int, hotX: Int, hotY: Int, pixels: [UInt8]) -> NSCursor? {
    guard width > 0, height > 0, pixels.count >= width * height * 4 else { return nil }
    var rgba = [UInt8](repeating: 0, count: width * height * 4)
    for i in 0..<(width * height) {
        let b = pixels[i * 4], g = pixels[i * 4 + 1], r = pixels[i * 4 + 2], a = pixels[i * 4 + 3]
        rgba[i * 4] = r
        rgba[i * 4 + 1] = g
        rgba[i * 4 + 2] = b
        rgba[i * 4 + 3] = a
    }
    guard let rep = NSBitmapImageRep(
        bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height,
        bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
        colorSpaceName: .deviceRGB, bytesPerRow: width * 4, bitsPerPixel: 32
    ), let dest = rep.bitmapData else { return nil }
    rgba.withUnsafeBufferPointer { buf in
        if let base = buf.baseAddress {
            dest.update(from: base, count: buf.count)
        }
    }
    let image = NSImage(size: NSSize(width: width, height: height))
    image.addRepresentation(rep)
    return NSCursor(image: image, hotSpot: NSPoint(x: hotX, y: hotY))
}
