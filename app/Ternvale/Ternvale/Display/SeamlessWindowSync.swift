import Foundation

/// Pure sync of guest window ids (testable without AppKit).
enum SeamlessWindowSync {
    static func removedIds(current: Set<UInt32>, incoming: Set<UInt32>) -> [UInt32] {
        current.subtracting(incoming).sorted()
    }

    static func addedIds(current: Set<UInt32>, incoming: Set<UInt32>) -> [UInt32] {
        incoming.subtracting(current).sorted()
    }
}
