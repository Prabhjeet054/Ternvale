import SwiftUI

/// Placeholder in the app window when `custom_display_pipeline` is false.
///
/// The live guest view is created in the VMM process. This app never links
/// Virtualization.framework.
struct VzGuestDisplayView: View {
    var body: some View {
        Color.black
    }
}
