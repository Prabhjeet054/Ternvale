import AppKit
import MetalKit

/// Crops a guest framebuffer, or shows a full per-window RGB24 capture.
final class SeamlessCropMetalView: MTKView, MTKViewDelegate {
    var cropRect: CGRect = .zero {
        didSet { needsDisplay = true }
    }

    private var queue: MTLCommandQueue?
    private var texture: MTLTexture?
    private var useFullTexture = false

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
        layer?.contentsScale = 1
    }

    @available(*, unavailable)
    required init(coder: NSCoder) {
        fatalError("init(coder:) has not been implemented")
    }

    func submit(frame: GuestDisplayFrame) {
        useFullTexture = false
        upload(width: Int(frame.width), height: Int(frame.height), format: metalFormat(frame.format), pixels: frame.pixels, stride: Int(frame.width) * 4)
    }

    func submitRgb24(width: UInt32, height: UInt32, pixels: Data) {
        useFullTexture = true
        var bgra = Data(count: Int(width) * Int(height) * 4)
        pixels.withUnsafeBytes { src in
            bgra.withUnsafeMutableBytes { dst in
                guard let s = src.bindMemory(to: UInt8.self).baseAddress,
                      let d = dst.bindMemory(to: UInt8.self).baseAddress
                else { return }
                let count = min(Int(width) * Int(height), src.count / 3)
                for i in 0..<count {
                    d[i * 4 + 0] = s[i * 3 + 2]
                    d[i * 4 + 1] = s[i * 3 + 1]
                    d[i * 4 + 2] = s[i * 3 + 0]
                    d[i * 4 + 3] = 255
                }
            }
        }
        upload(width: Int(width), height: Int(height), format: .bgra8Unorm, pixels: bgra, stride: Int(width) * 4)
        cropRect = CGRect(x: 0, y: 0, width: CGFloat(width), height: CGFloat(height))
    }

    func mtkView(_ view: MTKView, drawableSizeWillChange size: CGSize) {}

    func draw(in view: MTKView) {
        guard let drawable = currentDrawable,
              let queue,
              let texture,
              let commandBuffer = queue.makeCommandBuffer(),
              let blit = commandBuffer.makeBlitCommandEncoder()
        else { return }

        let ox: Int
        let oy: Int
        let cw: Int
        let ch: Int
        if useFullTexture {
            ox = 0
            oy = 0
            cw = texture.width
            ch = texture.height
        } else {
            ox = max(0, min(Int(cropRect.origin.x), texture.width - 1))
            oy = max(0, min(Int(cropRect.origin.y), texture.height - 1))
            cw = max(1, min(Int(cropRect.width), texture.width - ox))
            ch = max(1, min(Int(cropRect.height), texture.height - oy))
        }

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

    private func upload(width: Int, height: Int, format: MTLPixelFormat, pixels: Data, stride: Int) {
        guard width > 0, height > 0, !pixels.isEmpty, let device else { return }
        let desc = MTLTextureDescriptor.texture2DDescriptor(
            pixelFormat: format,
            width: width,
            height: height,
            mipmapped: false
        )
        desc.usage = [.shaderRead]
        guard let tex = device.makeTexture(descriptor: desc) else { return }
        pixels.withUnsafeBytes { raw in
            guard let base = raw.baseAddress else { return }
            tex.replace(
                region: MTLRegionMake2D(0, 0, width, height),
                mipmapLevel: 0,
                withBytes: base,
                bytesPerRow: stride
            )
        }
        texture = tex
        isPaused = false
        draw()
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
