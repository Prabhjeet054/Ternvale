import Foundation

/// Pure sync of guest window ids (testable without AppKit).
enum SeamlessWindowSync {
    static func removedIds(current: Set<UInt32>, incoming: Set<UInt32>) -> [UInt32] {
        current.subtracting(incoming).sorted()
    }

    static func addedIds(current: Set<UInt32>, incoming: Set<UInt32>) -> [UInt32] {
        incoming.subtracting(current).sorted()
    }

    /// Taskbar/Start are mapped to host chrome, not native guest windows.
    static func isHostMapped(_ kind: String?) -> Bool {
        switch kind {
        case "taskbar", "start":
            return false
        default:
            return true
        }
    }
}
