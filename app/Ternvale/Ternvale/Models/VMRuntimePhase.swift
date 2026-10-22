import Foundation

/// UI lifecycle for a VM (distinct from the control socket's `state` string).
enum VMRuntimePhase: Equatable {
    case stopped
    case starting
    case running
    case paused
    case stopping
    case crashed

    var label: String {
        switch self {
        case .stopped: "Stopped"
        case .starting: "Starting…"
        case .running: "Running"
        case .paused: "Paused"
        case .stopping: "Stopping…"
        case .crashed: "Crashed"
        }
    }
}
