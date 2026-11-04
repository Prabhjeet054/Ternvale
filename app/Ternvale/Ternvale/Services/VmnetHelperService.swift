import Foundation
import ServiceManagement
import os.log

/// Registers `ternvale-vmnetd` as a LaunchDaemon via SMAppService (macOS 13+).
///
/// Does not start or stop VMs. The VMM still talks to the helper over the Unix
/// socket documented in docs/NETWORKING.md.
enum VmnetHelperService {
    static let plistName = "com.ternvale.vmnetd.plist"
    private static let log = Logger(subsystem: "com.ternvale.app", category: "vmnet")

    static func registerIfNeeded() {
        let service = SMAppService.daemon(plistName: plistName)
        switch service.status {
        case .enabled:
            log.info("vmnet helper already enabled")
        case .requiresApproval:
            log.warning("vmnet helper needs Login Items approval")
            SMAppService.openSystemSettingsLoginItems()
        case .notRegistered, .notFound:
            do {
                try service.register()
                log.info("vmnet helper register() succeeded")
            } catch {
                log.error("vmnet helper register failed: \(error.localizedDescription, privacy: .public)")
            }
        @unknown default:
            log.error("vmnet helper unknown SMAppService status")
        }
    }
}
