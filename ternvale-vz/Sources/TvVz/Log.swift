import Foundation

enum TvLogLevel: Int32 {
    case error = 1
    case warn = 2
    case info = 3
    case debug = 4
    case trace = 5
}

typealias TvLogFn = @convention(c) (Int32, UnsafePointer<CChar>?) -> Void
typealias tv_vz_state_fn = @convention(c) (
    UnsafeMutableRawPointer?, Int32, UnsafePointer<CChar>?
) -> Void

enum TvLog {
    static var sink: TvLogFn?

    static func emit(_ level: TvLogLevel, _ message: String) {
        guard let sink else { return }
        message.withCString { sink(level.rawValue, $0) }
    }
}

@_cdecl("tv_vz_set_logger")
func tv_vz_set_logger(_ fn: TvLogFn?) {
    TvLog.sink = fn
}

func tvCopyError(_ message: String, _ err: UnsafeMutablePointer<CChar>?, _ errLen: Int) {
    guard let err, errLen > 0 else { return }
    let bytes = Array(message.utf8.prefix(errLen - 1)) + [0]
    for (i, b) in bytes.enumerated() {
        err[i] = CChar(bitPattern: b)
    }
}
