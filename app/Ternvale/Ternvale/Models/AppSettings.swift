import Foundation

/// User-facing toggles persisted in `UserDefaults`.
enum AppSettings {
    static let seamlessWindowsKey = "experimentalSeamlessWindows"
    static let dismissedUpdateVersionKey = "dismissedUpdateVersion"

    static var experimentalSeamlessWindows: Bool {
        get { UserDefaults.standard.bool(forKey: seamlessWindowsKey) }
        set { UserDefaults.standard.set(newValue, forKey: seamlessWindowsKey) }
    }

    static var dismissedUpdateVersion: String? {
        get { UserDefaults.standard.string(forKey: dismissedUpdateVersionKey) }
        set { UserDefaults.standard.set(newValue, forKey: dismissedUpdateVersionKey) }
    }
}
