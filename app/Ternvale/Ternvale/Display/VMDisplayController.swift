import AppKit
import Foundation
import os

struct GuestDisplayFrame: Equatable {
    let width: UInt32
    let height: UInt32
    let format: UInt32
    let sequence: UInt64
    let pixels: Data
}

/// Reads TVDP frames from the presenter and acks them after Metal presents.
final class VMDisplayController: ObservableObject {
    @Published private(set) var latestFrame: GuestDisplayFrame?
    @Published private(set) var guestPixelSize: CGSize = .zero
    @Published private(set) var isConnected = false
    @Published private(set) var statusText = "Display idle"

    let input = DisplayInputSender()

    private let log = Logger(subsystem: "com.ternvale.app", category: "display")
    private var readerTask: Task<Void, Never>?
    private var link: DisplaySocketLink?
    private var vmName: String?

    @MainActor
    func connect(vmName: String) {
        guard self.vmName != vmName || readerTask == nil else { return }
        disconnect()
        self.vmName = vmName
        statusText = "Waiting for display socket…"
        readerTask = Task.detached(priority: .userInitiated) { [weak self] in
            await self?.connectLoop(vmName: vmName)
        }
    }

    @MainActor
    func disconnect() {
        readerTask?.cancel()
        readerTask = nil
        link = nil
        isConnected = false
        latestFrame = nil
        guestPixelSize = .zero
        vmName = nil
        statusText = "Display idle"
    }

    func requestHostSize(width: UInt32, height: UInt32) {
        guard width > 0, height > 0 else { return }
        guard let link else { return }
        let value = UInt64(width) | UInt64(height) << 32
        if link.send(kind: PresentWire.kindSetSize, value: value) {
            log.info("requested guest mode \(width, privacy: .public)x\(height, privacy: .public)")
        } else {
            log.warning("SET_SIZE not sent; presenter socket closed")
        }
    }

    func acknowledgePresented(sequence: UInt64) {
        if link?.send(kind: PresentWire.kindPresented, value: sequence) == false {
            log.warning("could not ack frame \(sequence, privacy: .public) to presenter")
        } else {
            log.trace("acked frame sequence=\(sequence, privacy: .public)")
        }
    }

    private func connectLoop(vmName: String) async {
        let path = TernvalePaths.displaySocket(for: vmName).path
        while !Task.isCancelled {
            if FileManager.default.fileExists(atPath: path) {
                do {
                    let socket = try DisplaySocketLink(path: path)
                    guard socket.send(kind: PresentWire.kindHello, value: UInt64(PresentWire.version)) else {
                        try? await Task.sleep(nanoseconds: 200_000_000)
                        continue
                    }
                    await MainActor.run {
                        self.link = socket
                        self.input.attach(link: socket)
                        self.isConnected = true
                        self.statusText = "Connected"
                        self.log.info("display client ready for \(vmName, privacy: .public)")
                    }
                    await pump(link: socket)
                    await MainActor.run { self.disconnect() }
                    return
                } catch {
                    log.debug("display connect failed: \(error.localizedDescription, privacy: .public)")
                }
            }
            try? await Task.sleep(nanoseconds: 200_000_000)
        }
    }

    private func pump(link: DisplaySocketLink) async {
        while !Task.isCancelled {
            guard let raw = link.read(count: PresentWire.frameHeaderLen) else { break }
            do {
                let header = try PresentWireCodec.decodeFrameHeader(raw)
                var pixels = Data()
                if header.payloadLen > 0 {
                    guard let bytes = link.read(count: Int(header.payloadLen)) else { break }
                    pixels = Data(bytes)
                }
                let frame = GuestDisplayFrame(
                    width: header.width,
                    height: header.height,
                    format: header.format,
                    sequence: header.sequence,
                    pixels: pixels
                )
                await MainActor.run {
                    self.latestFrame = frame
                    if header.width > 0, header.height > 0 {
                        self.guestPixelSize = CGSize(
                            width: CGFloat(header.width),
                            height: CGFloat(header.height)
                        )
                    }
                }
            } catch {
                log.error("bad display frame: \(String(describing: error), privacy: .public)")
                break
            }
        }
    }
}
