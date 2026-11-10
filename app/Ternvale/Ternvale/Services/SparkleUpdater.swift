import Combine
import Foundation
import os

#if canImport(Sparkle)
    import Sparkle
#endif

/// Sparkle 2 driver. Automatic *install* is off unless the user opts in
/// (`AppSettings.autoInstallUpdates`). Checks still use the channel appcast
/// (stable vs beta) and require EdDSA + notarized Developer ID zips.
@MainActor
final class SparkleUpdater: ObservableObject {
    private let log = Logger(subsystem: "com.ternvale.app", category: "sparkle")

    #if canImport(Sparkle)
        private var controller: SPUStandardUpdaterController?
    #endif

    func start() {
        #if canImport(Sparkle)
            let controller = SPUStandardUpdaterController(
                startingUpdater: true,
                updaterDelegate: nil,
                userDriverDelegate: nil
            )
            applyConsent(to: controller)
            self.controller = controller
            log.info(
                "sparkle started channel=\(AppSettings.updateChannel, privacy: .public) autoInstall=\(AppSettings.autoInstallUpdates, privacy: .public)"
            )
        #else
            writeSparkleDefaults()
            log.info("sparkle not linked; JSON update banner still applies")
        #endif
    }

    func applyConsent() {
        #if canImport(Sparkle)
            if let controller {
                applyConsent(to: controller)
            }
        #endif
        Self.syncDefaults()
    }

    #if canImport(Sparkle)
        private func applyConsent(to controller: SPUStandardUpdaterController) {
            let consent = AppSettings.autoInstallUpdates
            controller.updater.automaticallyDownloadsUpdates = consent
            controller.updater.automaticallyChecksForUpdates = true
            Self.syncDefaults()
        }
    #endif

    static func syncDefaults() {
        let defaults = UserDefaults.standard
        defaults.set(AppSettings.autoInstallUpdates, forKey: "SUAutomaticallyUpdate")
        if let url = SparkleAppcast.feedURL(channel: AppSettings.updateChannel) {
            defaults.set(url.absoluteString, forKey: "SUFeedURL")
        }
    }

    private func writeSparkleDefaults() {
        Self.syncDefaults()
    }

    func checkForUpdates() {
        #if canImport(Sparkle)
            controller?.checkForUpdates(nil)
        #endif
    }
}
