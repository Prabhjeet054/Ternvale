import Foundation

/// User-facing toggles persisted in `UserDefaults`.
enum AppSettings {
    static let seamlessWindowsKey = "experimentalSeamlessWindows"
    static let dismissedUpdateVersionKey = "dismissedUpdateVersion"
    static let updateChannelKey = "updateChannel"
    static let autoInstallUpdatesKey = "autoInstallUpdates"
    static let crashReportingOptInKey = "crashReportingOptIn"

    static var experimentalSeamlessWindows: Bool {
        get { UserDefaults.standard.bool(forKey: seamlessWindowsKey) }
        set { UserDefaults.standard.set(newValue, forKey: seamlessWindowsKey) }
    }

    static var dismissedUpdateVersion: String? {
        get { UserDefaults.standard.string(forKey: dismissedUpdateVersionKey) }
        set { UserDefaults.standard.set(newValue, forKey: dismissedUpdateVersionKey) }
    }

    /// `stable` (default) or `beta`.
    static var updateChannel: String {
        get { UserDefaults.standard.string(forKey: updateChannelKey) ?? "stable" }
        set { UserDefaults.standard.set(newValue, forKey: updateChannelKey) }
    }

    /// Sparkle must not install without this. Default is false (`UserDefaults.bool`).
    static var autoInstallUpdates: Bool {
        get { UserDefaults.standard.bool(forKey: autoInstallUpdatesKey) }
        set { UserDefaults.standard.set(newValue, forKey: autoInstallUpdatesKey) }
    }

    /// Crash POST is skipped unless this is true **and** the preview Send button is used.
    static var crashReportingOptIn: Bool {
        get { UserDefaults.standard.bool(forKey: crashReportingOptInKey) }
        set { UserDefaults.standard.set(newValue, forKey: crashReportingOptInKey) }
    }
}
