import Foundation
import ServiceManagement
import os.log

/// Registers `ternvale-vmnetd` as a LaunchDaemon via SMAppService (macOS 13+).
///
/// Does not start or stop VMs. The VMM still talks to the helper over the Unix
/// socket documented in docs/NETWORKING.md.
enum VmnetHelperService {
    static let plistName = "com.ternvale.vmnetd.plist"
    /// Shown when bridged mode fails without Apple's restricted entitlement.
    static let bridgedEntitlementMissing = """
    Bridged networking needs Apple's restricted entitlement com.apple.vm.networking. \
    An ad-hoc or Development signature cannot grant it. Until Apple approves the request \
    in docs/ENTITLEMENT_REQUEST.md, use Shared (NAT) or Host-only, or run ternvale-vmnetd as root.
    """
    private static let log = Logger(subsystem: "com.ternvale.app", category: "vmnet")

    static func messageIfBridgedDenied(_ text: String) -> String? {
        let hay = text.lowercased()
        let hits = ["com.apple.vm.networking", "not_authorized", "invalid_access", "bridged mode needs"]
        guard hits.contains(where: { hay.contains($0) }) else { return nil }
        return bridgedEntitlementMissing
    }

    /// Names from `vmnet_copy_shared_interface_list` via `ternvale-vmnetd --list-bridges`.
    static func listBridgeableInterfaces(helperURL: URL?) -> [String] {
        guard let helper = helperURL ?? vmnetdURL() else {
            log.warning("ternvale-vmnetd not found; no bridgeable list")
            return []
        }
        let process = Process()
        process.executableURL = helper
        process.arguments = ["--list-bridges"]
        let pipe = Pipe()
        process.standardOutput = pipe
        process.standardError = Pipe()
        do {
            try process.run()
            process.waitUntilExit()
        } catch {
            log.error("list-bridges failed: \(error.localizedDescription, privacy: .public)")
            return []
        }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let text = String(data: data, encoding: .utf8) ?? ""
        let names = text.split(whereSeparator: \.isNewline).map(String.init).filter { !$0.isEmpty }
        log.info("bridgeable interfaces: \(names.joined(separator: ","), privacy: .public)")
        return names
    }

    static func vmnetdURL() -> URL? {
        let bundled = Bundle.main.bundleURL.appendingPathComponent("Contents/MacOS/ternvale-vmnetd")
        if FileManager.default.isExecutableFile(atPath: bundled.path) { return bundled }
        let dev = Bundle.main.bundleURL
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .deletingLastPathComponent()
            .appendingPathComponent("target/debug/ternvale-vmnetd")
        if FileManager.default.isExecutableFile(atPath: dev.path) { return dev }
        return nil
    }

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
