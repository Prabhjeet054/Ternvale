import Foundation

/// User-facing toggles persisted in `UserDefaults`.
enum AppSettings {
    static let seamlessWindowsKey = "experimentalSeamlessWindows"

    static var experimentalSeamlessWindows: Bool {
        get { UserDefaults.standard.bool(forKey: seamlessWindowsKey) }
        set { UserDefaults.standard.set(newValue, forKey: seamlessWindowsKey) }
    }
}
