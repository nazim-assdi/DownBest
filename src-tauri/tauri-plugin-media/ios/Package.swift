// swift-tools-version:5.3
import PackageDescription

let package = Package(
    name: "tauri-plugin-media",
    platforms: [.iOS(.v13), .macOS(.v10_13)],
    products: [
        .library(name: "tauri-plugin-media", type: .static, targets: ["tauri-plugin-media"])
    ],
    dependencies: [
        .package(name: "Tauri", path: "../../../../../crates/tauri/mobile/ios-api")
    ],
    targets: [
        .target(
            name: "tauri-plugin-media",
            dependencies: [.byName(name: "Tauri")],
            path: "Sources"
        )
    ]
)
