import AppKit
import SwiftUI

final class TernvaleAppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillTerminate(_ notification: Notification) {
        CrashHeartbeat.endClean()
    }
}

@main
struct TernvaleApp: App {
    @NSApplicationDelegateAdaptor(TernvaleAppDelegate.self) private var appDelegate

    init() {
        VmnetHelperService.registerIfNeeded()
        CrashHeartbeat.bootstrap()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .frame(minWidth: 720, minHeight: 480)
        }
        .commands {
            CommandGroup(replacing: .newItem) {}
            CommandGroup(after: .appInfo) {
                Button("Check for Updates…") {
                    NotificationCenter.default.post(name: .ternvaleCheckUpdates, object: nil)
                }
            }
            CommandGroup(after: .help) {
                Button("Send Feedback…") {
                    NotificationCenter.default.post(name: .ternvaleSendFeedback, object: nil)
                }
                Button("Crash & Update Preferences…") {
                    NotificationCenter.default.post(name: .ternvaleShowPreferences, object: nil)
                }
            }
        }
        Settings {
            PreferencesView()
        }
    }
}

extension Notification.Name {
    static let ternvaleCheckUpdates = Notification.Name("ternvaleCheckUpdates")
    static let ternvaleSendFeedback = Notification.Name("ternvaleSendFeedback")
    static let ternvaleShowPreferences = Notification.Name("ternvaleShowPreferences")
}
