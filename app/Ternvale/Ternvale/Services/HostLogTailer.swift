import Foundation
import os

/// Tails the newest host log file for a VM (`~/Library/Logs/Ternvale/ternvale-<name>-*.log`).
@MainActor
final class HostLogTailer: ObservableObject {
    @Published private(set) var text = ""
    @Published private(set) var path: URL?

    private let log = Logger(subsystem: "com.ternvale.app", category: "logs")
    private var timer: Timer?
    private var fileHandle: FileHandle?
    private var vmName: String?

    func follow(vmName: String?) {
        stop()
        self.vmName = vmName
        guard let vmName else {
            text = ""
            path = nil
            return
        }
        openLatest(for: vmName)
        timer = Timer.scheduledTimer(withTimeInterval: 0.35, repeats: true) { [weak self] _ in
            Task { @MainActor in
                self?.readChunk()
            }
        }
    }

    func stop() {
        timer?.invalidate()
        timer = nil
        try? fileHandle?.close()
        fileHandle = nil
    }

    private func openLatest(for vmName: String) {
        guard let url = TernvalePaths.newestHostLog(for: vmName) else {
            text = "No host log yet for \(vmName).\nStart the VM to create ~/Library/Logs/Ternvale/ternvale-\(vmName)-<timestamp>.log\n"
            path = nil
            return
        }
        if path == url, fileHandle != nil { return }
        try? fileHandle?.close()
        path = url
        do {
            let handle = try FileHandle(forReadingFrom: url)
            fileHandle = handle
            if let attrs = try? FileManager.default.attributesOfItem(atPath: url.path),
               let size = attrs[.size] as? UInt64, size > 32_768 {
                try handle.seek(toOffset: size - 32_768)
            }
            readChunk()
            log.debug("tailing \(url.path, privacy: .public)")
        } catch {
            text = "Could not open log: \(error.localizedDescription)\n"
        }
    }

    private func readChunk() {
        guard let vmName else { return }
        if fileHandle == nil {
            openLatest(for: vmName)
            return
        }
        guard let handle = fileHandle else { return }
        let data = handle.readDataToEndOfFile()
        if !data.isEmpty, let chunk = String(data: data, encoding: .utf8) {
            text.append(chunk)
            if text.count > 200_000 {
                text = String(text.suffix(180_000))
            }
        }
        if let newest = TernvalePaths.newestHostLog(for: vmName), newest != path {
            openLatest(for: vmName)
        }
    }
}
