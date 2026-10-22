import Darwin
import Foundation
import os

/// Step 28 control socket: one JSON object per line each way.
final class ControlSocketClient {
    private let log = Logger(subsystem: "com.ternvale.app", category: "control")

    enum ReachError: Error, Equatable {
        case notRunning
        case staleSocket
    }

    func status(vmName: String) throws -> ControlResponse {
        try request(vmName: vmName, body: ["cmd": "status"])
    }

    func pause(vmName: String, timeoutMs: UInt64 = 5_000) throws -> ControlResponse {
        try request(vmName: vmName, body: ["cmd": "pause", "timeout_ms": timeoutMs])
    }

    func resume(vmName: String) throws -> ControlResponse {
        try request(vmName: vmName, body: ["cmd": "resume"])
    }

    func shutdown(vmName: String) throws -> ControlResponse {
        try request(vmName: vmName, body: ["cmd": "shutdown"])
    }

    func forceStop(vmName: String) throws -> ControlResponse {
        try request(vmName: vmName, body: ["cmd": "force-stop"])
    }

    func isReachable(vmName: String) -> Bool {
        (try? connect(vmName: vmName)) != nil
    }

    private func request(vmName: String, body: [String: Any]) throws -> ControlResponse {
        let socket = try connect(vmName: vmName)
        defer { close(socket) }
        let payload = try JSONSerialization.data(withJSONObject: body)
        var line = payload
        line.append(0x0A)
        let sent = line.withUnsafeBytes { ptr in
            write(socket, ptr.baseAddress, ptr.count)
        }
        if sent < 0 {
            throw POSIXError(.init(rawValue: errno) ?? .EIO)
        }
        var buffer = Data()
        var chunk = [UInt8](repeating: 0, count: 4096)
        while true {
            let n = read(socket, &chunk, chunk.count)
            if n <= 0 { break }
            buffer.append(chunk, count: n)
            if buffer.contains(0x0A) { break }
            if buffer.count > 64 * 1024 {
                throw POSIXError(.EMSGSIZE)
            }
        }
        guard let newline = buffer.firstIndex(of: 0x0A) else {
            throw POSIXError(.EIO)
        }
        let json = buffer[..<newline]
        let response = try JSONDecoder().decode(ControlResponse.self, from: json)
        log.debug("control \(body["cmd"] as? String ?? "?") ok=\(response.ok, privacy: .public)")
        return response
    }

    private func connect(vmName: String) throws -> Int32 {
        let path = TernvalePaths.controlSocket(for: vmName).path
        guard FileManager.default.fileExists(atPath: path) else {
            throw ReachError.notRunning
        }
        let fd = socket(AF_UNIX, SOCK_STREAM, 0)
        if fd < 0 {
            throw POSIXError(.init(rawValue: errno) ?? .EIO)
        }
        var addr = sockaddr_un()
        addr.sun_family = sa_family_t(AF_UNIX)
        let maxLen = MemoryLayout.size(ofValue: addr.sun_path) - 1
        guard path.utf8.count <= maxLen else {
            close(fd)
            throw POSIXError(.ENAMETOOLONG)
        }
        path.withCString { cstr in
            withUnsafeMutablePointer(to: &addr.sun_path) { ptr in
                ptr.withMemoryRebound(to: CChar.self, capacity: maxLen + 1) { dest in
                    strncpy(dest, cstr, maxLen)
                }
            }
        }
        let size = socklen_t(MemoryLayout<sockaddr_un>.size)
        let result = withUnsafePointer(to: &addr) { ptr in
            ptr.withMemoryRebound(to: sockaddr.self, capacity: 1) { sa in
                Darwin.connect(fd, sa, size)
            }
        }
        if result != 0 {
            close(fd)
            if errno == ECONNREFUSED {
                throw ReachError.staleSocket
            }
            throw POSIXError(.init(rawValue: errno) ?? .EIO)
        }
        return fd
    }
}

struct ControlResponse: Decodable {
    let ok: Bool
    let error: String?
    let status: VMStatusPayload?
}

struct VMStatusPayload: Decodable {
    let name: String
    let state: String
    let cpus: UInt32
    let uptimeMs: UInt64
    let failure: String?
    let stopCause: String?

    enum CodingKeys: String, CodingKey {
        case name, state, cpus, failure
        case uptimeMs = "uptime_ms"
        case stopCause = "stop_cause"
    }
}
