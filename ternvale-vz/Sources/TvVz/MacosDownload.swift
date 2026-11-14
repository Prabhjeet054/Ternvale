import Foundation
import CryptoKit

private func cString(_ ptr: UnsafePointer<CChar>?) -> String {
    guard let ptr else { return "" }
    return String(cString: ptr)
}

private func kv(_ pairs: [String: String]) -> String {
    pairs.map { "\($0.key)=\($0.value.replacingOccurrences(of: "\n", with: " "))" }
        .sorted()
        .joined(separator: "\n")
}

private func copyOut(_ text: String, _ out: UnsafeMutablePointer<CChar>?, _ len: Int) {
    guard let out, len > 0 else { return }
    let bytes = Array(text.utf8.prefix(len - 1)) + [0]
    for (i, b) in bytes.enumerated() {
        out[i] = CChar(bitPattern: b)
    }
}

private func hexEncode(_ data: Data) -> String {
    data.map { String(format: "%02x", $0) }.joined()
}

/// Stream an IPSW into `dest.part` so a killed transfer can resume with Range.
/// SHA-256 is CryptoKit over the file, not a mapped IPSW.
@_cdecl("tv_vz_macos_download")
func tv_vz_macos_download(
    _ url: UnsafePointer<CChar>?,
    _ dest: UnsafePointer<CChar>?,
    _ expectedSha: UnsafePointer<CChar>?,
    _ cb: tv_vz_progress_fn?,
    _ ctx: UnsafeMutableRawPointer?,
    _ out: UnsafeMutablePointer<CChar>?,
    _ outLen: Int,
    _ err: UnsafeMutablePointer<CChar>?,
    _ errLen: Int
) -> Int32 {
    let destPath = cString(dest)
    let expected = cString(expectedSha).lowercased()
    guard let remote = URL(string: cString(url)) else {
        tvCopyError("invalid restore URL", err, errLen)
        return -1
    }
    let finalURL = URL(fileURLWithPath: destPath)
    if FileManager.default.fileExists(atPath: destPath),
       let hashed = sha256File(destPath)
    {
        if expected.isEmpty || hashed.hex == expected {
            copyOut(
                kv(["sha256": hashed.hex, "bytes": String(hashed.bytes), "resumed": "0"]),
                out,
                outLen
            )
            return 0
        }
    }
    let part = destPath + ".part"
    if !FileManager.default.fileExists(atPath: part) {
        FileManager.default.createFile(atPath: part, contents: nil)
    }
    do {
        let handle = try FileHandle(forWritingTo: URL(fileURLWithPath: part))
        let started = try handle.seekToEnd()
        var request = URLRequest(url: remote)
        request.timeoutInterval = 3600
        if started > 0 {
            request.setValue("bytes=\(started)-", forHTTPHeaderField: "Range")
        }
        let streamer = StreamDownload(
            handle: handle,
            have: started,
            startedWith: started,
            cb: cb,
            ctx: ctx
        )
        let config = URLSessionConfiguration.ephemeral
        config.timeoutIntervalForRequest = 3600
        config.timeoutIntervalForResource = 86_400
        let queue = OperationQueue()
        queue.maxConcurrentOperationCount = 1
        let session = URLSession(configuration: config, delegate: streamer, delegateQueue: queue)
        session.dataTask(with: request).resume()
        streamer.done.wait()
        session.finishTasksAndInvalidate()
        try handle.close()
        if let fail = streamer.fail {
            tvCopyError(fail, err, errLen)
            return -1
        }
        guard let hashed = sha256File(part) else {
            tvCopyError("hash part", err, errLen)
            return -1
        }
        if !expected.isEmpty && hashed.hex != expected {
            tvCopyError("sha256 mismatch (got \(hashed.hex))", err, errLen)
            return -1
        }
        try? FileManager.default.removeItem(at: finalURL)
        try FileManager.default.moveItem(at: URL(fileURLWithPath: part), to: finalURL)
        let resumed = started > 0 && !streamer.replaced
        copyOut(
            kv(["sha256": hashed.hex, "bytes": String(hashed.bytes), "resumed": resumed ? "1" : "0"]),
            out,
            outLen
        )
        TvLog.emit(.info, "macos ipsw downloaded bytes=\(hashed.bytes) resumed=\(resumed)")
        return 0
    } catch {
        tvCopyError(error.localizedDescription, err, errLen)
        return -1
    }
}

private final class StreamDownload: NSObject, URLSessionDataDelegate {
    let handle: FileHandle
    var have: UInt64
    let startedWith: UInt64
    var expected: UInt64
    var fail: String?
    var replaced = false
    let done = DispatchSemaphore(value: 0)
    let cb: tv_vz_progress_fn?
    let ctx: UnsafeMutableRawPointer?

    init(
        handle: FileHandle,
        have: UInt64,
        startedWith: UInt64,
        cb: tv_vz_progress_fn?,
        ctx: UnsafeMutableRawPointer?
    ) {
        self.handle = handle
        self.have = have
        self.startedWith = startedWith
        self.expected = have
        self.cb = cb
        self.ctx = ctx
    }

    func urlSession(
        _ session: URLSession,
        dataTask: URLSessionDataTask,
        didReceive response: URLResponse,
        completionHandler: @escaping (URLSession.ResponseDisposition) -> Void
    ) {
        let http = response as? HTTPURLResponse
        let code = http?.statusCode ?? 0
        let length = http.flatMap { $0.value(forHTTPHeaderField: "Content-Length") }.flatMap(UInt64.init) ?? 0
        if code == 200 && startedWith > 0 {
            do {
                try handle.truncate(atOffset: 0)
                have = 0
                replaced = true
            } catch {
                fail = error.localizedDescription
                completionHandler(.cancel)
                return
            }
        }
        if !(200...206).contains(code) {
            fail = "http \(code)"
            completionHandler(.cancel)
            return
        }
        if code == 206 {
            expected = startedWith &+ length
        } else if length > 0 {
            expected = length
        }
        TvLog.emit(.debug, "macos ipsw http=\(code) have=\(have) expected=\(expected)")
        completionHandler(.allow)
    }

    func urlSession(_ session: URLSession, dataTask: URLSessionDataTask, didReceive data: Data) {
        do {
            try handle.write(contentsOf: data)
            have += UInt64(data.count)
            let permille: UInt32
            if expected > 0 {
                permille = UInt32(min(1000, (have &* 1000) / expected))
            } else {
                permille = 0
            }
            cb?(ctx, have, expected, permille)
        } catch {
            fail = error.localizedDescription
        }
    }

    func urlSession(_ session: URLSession, task: URLSessionTask, didCompleteWithError error: Error?) {
        if let error {
            fail = error.localizedDescription
        }
        done.signal()
    }
}

struct FileHash {
    let hex: String
    let bytes: UInt64
}

func sha256File(_ path: String) -> FileHash? {
    guard let handle = FileHandle(forReadingAtPath: path) else { return nil }
    defer { try? handle.close() }
    var hasher = SHA256()
    var bytes: UInt64 = 0
    while true {
        let chunk: Data
        do {
            chunk = try handle.read(upToCount: 1024 * 1024) ?? Data()
        } catch {
            return nil
        }
        if chunk.isEmpty { break }
        hasher.update(data: chunk)
        bytes += UInt64(chunk.count)
    }
    return FileHash(hex: hexEncode(Data(hasher.finalize())), bytes: bytes)
}
