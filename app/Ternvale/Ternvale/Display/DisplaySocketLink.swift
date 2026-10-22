import Darwin
import Foundation
import os

/// Unix stream to the VMM presenter (`*.display.sock`).
final class DisplaySocketLink {
    private let log = Logger(subsystem: "com.ternvale.app", category: "display")
    private let fd: Int32
    private let writeLock = NSLock()

    init(path: String) throws {
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        guard fd >= 0 else {
            throw POSIXError(POSIXErrorCode(rawValue: errno) ?? .EIO)
        }
        var addr = sockaddr_un()
        let bytes = Array(path.utf8)
        guard bytes.count < MemoryLayout.size(ofValue: addr.sun_path) else {
            close(fd)
            throw DisplayLinkError.pathTooLong
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
        log.debug("connected display socket \(path, privacy: .public)")
    }

    deinit {
        close(fd)
    }

    func read(count: Int) -> [UInt8]? {
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

    func send(kind: UInt16, value: UInt64) -> Bool {
        let msg = PresentWireCodec.encodeMessage(kind: kind, value: value)
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

    enum DisplayLinkError: Error {
        case pathTooLong
    }
}
