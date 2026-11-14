import SwiftUI

struct VMDisplayPanel: View {
    @ObservedObject var controller: VMDisplayController
    @ObservedObject var seamless: SeamlessWindowsCoordinator
    let isActive: Bool
    /// When false, skip Metal (`custom_display_pipeline`); VZ owns the pixels.
    var usesCustomDisplayPipeline: Bool = true

    @State private var lastRequestedSize: CGSize = .zero

    var body: some View {
        GeometryReader { geo in
            ZStack {
                Color.black
                if isActive, seamless.isActive {
                    VStack(spacing: 8) {
                        Text("Experimental seamless mode")
                            .font(.headline)
                        Text("\(seamless.windowCount) guest window(s) on screen")
                            .foregroundStyle(.secondary)
                        Text("Embedded display hidden; crops come from the shared framebuffer.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                            .multilineTextAlignment(.center)
                    }
                    .padding()
                } else if isActive, !usesCustomDisplayPipeline {
                    VStack(spacing: 10) {
                        VzGuestDisplayView()
                            .frame(maxWidth: .infinity, maxHeight: 8)
                        Text("Display is owned by Virtualization.framework")
                            .font(.headline)
                        Text(
                            "The guest is in the Ternvale VZ window. Resize that window to change resolution on macOS 14+ (`automaticallyReconfiguresDisplay`). The app skips the Metal viewer for this backend."
                        )
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .multilineTextAlignment(.center)
                        .padding(.horizontal)
                    }
                } else if isActive {
                    VMMetalDisplayView(controller: controller)
                } else {
                    Text("Start the VM to see the display")
                        .foregroundStyle(.secondary)
                }
                VStack {
                    HStack {
                        Text(controller.statusText)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Spacer()
                        if controller.guestPixelSize != .zero {
                            Text(
                                "\(Int(controller.guestPixelSize.width))×\(Int(controller.guestPixelSize.height))"
                            )
                            .font(.caption.monospacedDigit())
                            .foregroundStyle(.secondary)
                        }
                    }
                    .padding(6)
                    Spacer()
                }
            }
            .onChange(of: geo.size) { _, size in
                requestResizeIfNeeded(viewSize: size)
            }
            .onAppear {
                requestResizeIfNeeded(viewSize: geo.size)
            }
        }
        .frame(minHeight: 240)
    }

    private func requestResizeIfNeeded(viewSize: CGSize) {
        guard isActive, usesCustomDisplayPipeline, controller.isConnected else { return }
        let scale = NSScreen.main?.backingScaleFactor ?? 2
        let w = UInt32(max(64, (viewSize.width * scale).rounded(.down)))
        let h = UInt32(max(64, (viewSize.height * scale).rounded(.down)))
        let size = CGSize(width: CGFloat(w), height: CGFloat(h))
        guard size != lastRequestedSize else { return }
        lastRequestedSize = size
        controller.requestHostSize(width: w, height: h)
    }
}
