import AppKit
import SwiftUI

struct UpdateBannerView: View {
    let banner: UpdateChecker.Banner
    let onDismiss: () -> Void

    var body: some View {
        HStack(alignment: .firstTextBaseline, spacing: 12) {
            VStack(alignment: .leading, spacing: 2) {
                Text("New version available")
                    .font(.headline)
                Text("\(banner.remoteVersion) is out (you have \(banner.currentVersion)).")
                    .font(.subheadline)
                if let notes = banner.notes, !notes.isEmpty {
                    Text(notes)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
            Spacer()
            if let url = banner.learnMore {
                Button("Learn more") {
                    NSWorkspace.shared.open(url)
                }
            }
            Button("Dismiss", action: onDismiss)
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(.yellow.opacity(0.35))
    }
}
