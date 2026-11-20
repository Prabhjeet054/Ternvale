import AppKit
import Darwin
import Foundation
import os
import SwiftUI

struct GuestWindowRecord: Codable, Hashable, Equatable {
    let id: UInt32
    let title: String
    let x: Int32
    let y: Int32
    let width: UInt32
    let height: UInt32
    let zOrder: UInt32?
    let state: String?
    let focused: Bool?
    let parentId: UInt32?
    let kind: String?
}

private struct SeamlessFrameMeta: Codable {
    let id: UInt32
    let seq: UInt64
    let width: UInt32
    let height: UInt32
    let format: String
    let path: String
}

private struct SeamlessUpdate: Codable {
    let windows: [GuestWindowRecord]
    let sequence: UInt64
    let frames: [SeamlessFrameMeta]?
}

/// Host compositor: one native window per guest app window (gated).
@MainActor
final class SeamlessWindowsCoordinator: ObservableObject {
    @Published private(set) var isActive = false
    @Published private(set) var windowCount = 0
    @Published private(set) var shellHint = ""

    private let display: VMDisplayController
    private let log = Logger(subsystem: "com.ternvale.app", category: "seamless")
    private var readerTask: Task<Void, Never>?
    private var frameTask: Task<Void, Never>?
    private var hosts: [UInt32: SeamlessGuestWindowHost] = [:]
    private var lastRecords: [UInt32: GuestWindowRecord] = [:]
    private var socket: SeamlessLineSocket?

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
        socket = nil
        for host in hosts.values {
            host.close()
        }
        hosts.removeAll()
        lastRecords.removeAll()
        windowCount = 0
        shellHint = ""
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
                    await MainActor.run { self.socket = client }
                    log.debug("connected seamless socket \(path, privacy: .public)")
                    while !Task.isCancelled {
                        guard let line = client.readLine() else { break }
                        guard let data = line.data(using: .utf8) else { continue }
                        let decoder = JSONDecoder()
                        decoder.keyDecodingStrategy = .convertFromSnakeCase
                        let update = try decoder.decode(SeamlessUpdate.self, from: data)
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
        let mapped = update.windows.filter { SeamlessWindowSync.isHostMapped($0.kind) }
        let shell = update.windows.filter { !SeamlessWindowSync.isHostMapped($0.kind) }
        shellHint = shell.map(\.title).filter { !$0.isEmpty }.joined(separator: " · ")
        let incomingSet = Set(mapped.map(\.id))
        let currentSet = Set(hosts.keys)
        for id in SeamlessWindowSync.removedIds(current: currentSet, incoming: incomingSet) {
            hosts[id]?.close()
            hosts.removeValue(forKey: id)
            lastRecords.removeValue(forKey: id)
            log.info("closed seamless host window for guest id=\(id, privacy: .public) (guest window gone)")
        }
        let send: (String) -> Void = { [weak self] line in
            self?.socket?.writeLine(line)
        }
        for record in mapped {
            if let host = hosts[record.id] {
                if lastRecords[record.id] != record {
                    host.update(record: record, guestSize: display.guestPixelSize)
                    lastRecords[record.id] = record
                }
            } else {
                let host = SeamlessGuestWindowHost(
                    record: record,
                    guestSize: display.guestPixelSize,
                    send: send
                )
                hosts[record.id] = host
                lastRecords[record.id] = record
                log.info(
                    "opened seamless window id=\(record.id, privacy: .public) title=\(record.title, privacy: .public) kind=\(record.kind ?? "app", privacy: .public)"
                )
                if let frame = display.latestFrame {
                    host.submit(frame: frame)
                }
            }
        }
        for record in mapped {
            if let parent = record.parentId, let parentHost = hosts[parent], let child = hosts[record.id] {
                child.attachParent(parentHost.window)
            }
        }
        if let frames = update.frames {
            for meta in frames {
                guard let host = hosts[meta.id] else { continue }
                guard meta.format == "rgb24" else { continue }
                guard let data = try? Data(contentsOf: URL(fileURLWithPath: meta.path)) else { continue }
                let expected = Int(meta.width) * Int(meta.height) * 3
                guard data.count == expected, expected > 0, expected <= 8 * 1024 * 1024 else {
                    log.debug("rejected seamless frame id=\(meta.id, privacy: .public) bytes=\(data.count, privacy: .public)")
                    continue
                }
                host.submitCapture(width: meta.width, height: meta.height, rgb24: data)
            }
        }
        windowCount = hosts.count
        if mapped.isEmpty {
            log.info("seamless guest list empty; embedded single-window display stays visible")
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

    func writeLine(_ line: String) {
        var payload = Array(line.utf8)
        payload.append(0x0A)
        _ = payload.withUnsafeBytes { Darwin.write(fd, $0.baseAddress, $0.count) }
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
