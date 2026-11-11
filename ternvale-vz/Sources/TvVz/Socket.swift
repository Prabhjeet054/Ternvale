import Foundation
import Virtualization
import Darwin

/// Forwards a guest connect on a virtio-vsock port to the Unix socket a host
/// service listens on (`host-<port>.sock`), matching the Step 29 agent layout.
///
/// VZVirtioSocketListener / Delegate: macOS 11.0.
final class UnixVsockListener: NSObject, VZVirtioSocketListenerDelegate {
    let unixPath: String
    let listener = VZVirtioSocketListener()
    private var connections: [VZVirtioSocketConnection] = []

    init(unixPath: String) {
        self.unixPath = unixPath
        super.init()
        listener.delegate = self
    }

    func listener(
        _ listener: VZVirtioSocketListener,
        shouldAcceptNewConnection connection: VZVirtioSocketConnection,
        from socketDevice: VZVirtioSocketDevice
    ) -> Bool {
        TvLog.emit(.info, "vsock accept path=\(unixPath)")
        guard let unix = connectUnix(path: unixPath) else {
            TvLog.emit(.warn, "vsock unix connect failed path=\(unixPath)")
            connection.close()
            return false
        }
        let guestFd = Darwin.dup(connection.fileDescriptor)
        if guestFd < 0 {
            connection.close()
            unix.closeFile()
            return false
        }
        let guest = FileHandle(fileDescriptor: guestFd, closeOnDealloc: true)
        connections.append(connection)
        splice(guest: guest, host: unix)
        return true
    }
}

private func connectUnix(path: String) -> FileHandle? {
    let fd = Darwin.socket(AF_UNIX, SOCK_STREAM, 0)
    if fd < 0 {
        return nil
    }
    var addr = sockaddr_un()
    addr.sun_family = sa_family_t(AF_UNIX)
    let chars = Array(path.utf8)
    if chars.count >= MemoryLayout.size(ofValue: addr.sun_path) {
        Darwin.close(fd)
        return nil
    }
    withUnsafeMutableBytes(of: &addr.sun_path) { buf in
        for (i, b) in chars.enumerated() {
            buf[i] = b
        }
        buf[chars.count] = 0
    }
    let ok = withUnsafePointer(to: &addr) { ptr in
        ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sock in
            Darwin.connect(fd, sock, socklen_t(MemoryLayout<sockaddr_un>.size))
        }
    }
    if ok != 0 {
        Darwin.close(fd)
        return nil
    }
    return FileHandle(fileDescriptor: fd, closeOnDealloc: true)
}

private func splice(guest: FileHandle, host: FileHandle) {
    guest.readabilityHandler = { src in
        let data = src.availableData
        if data.isEmpty {
            src.readabilityHandler = nil
            host.readabilityHandler = nil
            src.closeFile()
            host.closeFile()
            return
        }
        host.write(data)
    }
    host.readabilityHandler = { src in
        let data = src.availableData
        if data.isEmpty {
            src.readabilityHandler = nil
            guest.readabilityHandler = nil
            src.closeFile()
            guest.closeFile()
            return
        }
        guest.write(data)
    }
}
