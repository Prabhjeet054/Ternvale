import SwiftUI

struct LogPanelView: View {
    let text: String
    let path: URL?

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text("Host log")
                    .font(.headline)
                if let path {
                    Text(path.lastPathComponent)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                }
                Spacer()
            }
            .padding(.horizontal)
            .padding(.top, 8)
            ScrollView {
                ScrollViewReader { proxy in
                    Text(text.isEmpty ? " " : text)
                        .font(.system(.caption, design: .monospaced))
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(8)
                        .id("bottom")
                        .onChange(of: text) { _, _ in
                            proxy.scrollTo("bottom", anchor: .bottom)
                        }
                }
            }
            .background(Color(nsColor: .textBackgroundColor))
        }
    }
}
