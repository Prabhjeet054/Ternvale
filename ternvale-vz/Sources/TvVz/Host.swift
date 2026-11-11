import Foundation
import Virtualization

/// Maps VZVirtualMachine.state (macOS 11) onto ternvale_vmm::VmState integers:
/// created=0 running=1 paused=2 stopping=3 stopped=4 failed=5.
enum TvVmState: Int32 {
    case created = 0
    case running = 1
    case paused = 2
    case stopping = 3
    case stopped = 4
    case failed = 5
}

final class TvVzSession: NSObject, VZVirtualMachineDelegate {
    let name: String
    let queue: DispatchQueue
    var vm: VZVirtualMachine?
    var lastState: TvVmState = .created
    var stateCb: tv_vz_state_fn?
    var ctx: UnsafeMutableRawPointer?
    var udsDir: String = ""
    var vsockPorts: [UInt32] = []
    var vsockProxies: [UnixVsockListener] = []
    /// Held so the VZ serial `FileHandle` is not closed while the VM runs.
    var serialWriter: FileHandle?
    var serialReader: FileHandle?

    init(name: String) {
        self.name = name
        // Apple: every VZVirtualMachine method must run on the queue passed
        // to init(configuration:queue:) (Virtualization.framework, macOS 12).
        self.queue = DispatchQueue(label: "ternvale.vz.\(name)")
        super.init()
    }

    func emit(_ state: TvVmState, detail: String) {
        lastState = state
        TvLog.emit(.info, "vm=\(name) vz state=\(state.rawValue) \(detail)")
        if let cb = stateCb {
            detail.withCString { cb(ctx, state.rawValue, $0) }
        }
    }

    // VZVirtualMachineDelegate — macOS 11.
    func guestDidStop(_ virtualMachine: VZVirtualMachine) {
        emit(.stopped, detail: "guestDidStop")
    }

    func virtualMachine(_ virtualMachine: VZVirtualMachine, didStopWithError error: Error) {
        emit(.failed, detail: "didStopWithError \(error.localizedDescription)")
    }

    override func observeValue(
        forKeyPath keyPath: String?,
        of object: Any?,
        change: [NSKeyValueChangeKey: Any]?,
        context: UnsafeMutableRawPointer?
    ) {
        guard keyPath == "state", let vm else { return }
        emit(mapVzState(vm.state), detail: "kvo")
    }
}

func mapVzState(_ state: VZVirtualMachine.State) -> TvVmState {
    switch state {
    case .stopped: return .stopped
    case .running: return .running
    case .paused: return .paused
    case .error: return .failed
    case .starting, .resuming: return .running
    case .pausing, .stopping: return .stopping
    case .saving, .restoring: return .paused
    @unknown default: return .failed
    }
}

enum Registry {
    static let lock = NSLock()
    static var next: Int32 = 1
    static var sessions: [Int32: TvVzSession] = [:]

    static func put(_ session: TvVzSession) -> Int32 {
        lock.lock()
        defer { lock.unlock() }
        let id = next
        next += 1
        sessions[id] = session
        return id
    }

    static func get(_ id: Int32) -> TvVzSession? {
        lock.lock()
        defer { lock.unlock() }
        return sessions[id]
    }

    static func take(_ id: Int32) -> TvVzSession? {
        lock.lock()
        defer { lock.unlock() }
        return sessions.removeValue(forKey: id)
    }
}
