import SwiftUI

@main
struct TernvaleApp: App {
    init() {
        VmnetHelperService.registerIfNeeded()
    }

    var body: some Scene {
        WindowGroup {
            ContentView()
                .frame(minWidth: 720, minHeight: 480)
        }
    }
}
