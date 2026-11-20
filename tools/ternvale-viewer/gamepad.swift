// GameController.framework → TVDP gamepad messages (Linux BTN_*/ABS_* codes).
// Rumble is not sent: virtio-input statusq is LED-only (TODO(verify) EV_FF).

import AppKit
import GameController

private let btnSouth: UInt16 = 0x130
private let btnEast: UInt16 = 0x131
private let btnNorth: UInt16 = 0x133
private let btnWest: UInt16 = 0x134
private let btnTl: UInt16 = 0x136
private let btnTr: UInt16 = 0x137
private let btnSelect: UInt16 = 0x13a
private let btnStart: UInt16 = 0x13b
private let btnMode: UInt16 = 0x13c
private let btnThumbL: UInt16 = 0x13d
private let btnThumbR: UInt16 = 0x13e
private let absX: UInt16 = 0x00
private let absY: UInt16 = 0x01
private let absZ: UInt16 = 0x02
private let absRx: UInt16 = 0x03
private let absRy: UInt16 = 0x04
private let absRz: UInt16 = 0x05
private let absHatX: UInt16 = 0x10
private let absHatY: UInt16 = 0x11

final class GamepadWatcher {
    static let shared = GamepadWatcher()
    private var input: InputSender?

    func attach(input: InputSender) {
        self.input = input
        NotificationCenter.default.addObserver(
            forName: .GCControllerDidConnect, object: nil, queue: .main
        ) { [weak self] note in
            if let pad = note.object as? GCController { self?.bind(pad) }
        }
        for pad in GCController.controllers() { bind(pad) }
    }

    private func bind(_ pad: GCController) {
        guard let gp = pad.extendedGamepad, let input else { return }
        let stick = { (v: Float) -> Int32 in
            Int32((Double(v) * 32767.0).rounded())
        }
        let trigger = { (v: Float) -> Int32 in
            Int32((Double(v) * 255.0).rounded())
        }
        gp.valueChangedHandler = { gp, _ in
            input.gamepadButton(btnSouth, gp.buttonA.isPressed)
            input.gamepadButton(btnEast, gp.buttonB.isPressed)
            input.gamepadButton(btnWest, gp.buttonX.isPressed)
            input.gamepadButton(btnNorth, gp.buttonY.isPressed)
            input.gamepadButton(btnTl, gp.leftShoulder.isPressed)
            input.gamepadButton(btnTr, gp.rightShoulder.isPressed)
            input.gamepadButton(btnSelect, gp.buttonOptions?.isPressed ?? false)
            input.gamepadButton(btnStart, gp.buttonMenu.isPressed)
            input.gamepadButton(btnMode, gp.buttonHome?.isPressed ?? false)
            input.gamepadButton(btnThumbL, gp.leftThumbstickButton?.isPressed ?? false)
            input.gamepadButton(btnThumbR, gp.rightThumbstickButton?.isPressed ?? false)
            // Linux ABS_Y negative is typically up; GameController y is up = +1.
            input.gamepadAxis(absX, stick(gp.leftThumbstick.xAxis.value))
            input.gamepadAxis(absY, stick(-gp.leftThumbstick.yAxis.value))
            input.gamepadAxis(absRx, stick(gp.rightThumbstick.xAxis.value))
            input.gamepadAxis(absRy, stick(-gp.rightThumbstick.yAxis.value))
            input.gamepadAxis(absZ, trigger(gp.leftTrigger.value))
            input.gamepadAxis(absRz, trigger(gp.rightTrigger.value))
            input.gamepadAxis(absHatX, Int32(gp.dpad.xAxis.value.rounded()))
            input.gamepadAxis(absHatY, Int32((-gp.dpad.yAxis.value).rounded()))
        }
    }
}

func nsCursor(width: Int, height: Int, hotX: Int, hotY: Int, pixels: [UInt8]) -> NSCursor? {
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
    let hot = NSPoint(x: CGFloat(hotX), y: CGFloat(hotY))
    return NSCursor(image: image, hotSpot: hot)
}
