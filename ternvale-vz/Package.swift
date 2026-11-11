// swift-tools-version: 5.9
// Static Virtualization.framework host. Linked from Rust via the C ABI in
// include/tv_vz.h (not swift-bridge: we own the queue hopping and the
// exported surface stays a handful of C functions).
import PackageDescription

let package = Package(
    name: "TvVz",
    platforms: [.macOS(.v13)],
    products: [
        .library(name: "TvVz", type: .static, targets: ["TvVz"]),
    ],
    targets: [
        .target(name: "TvVz", path: "Sources/TvVz"),
    ]
)
