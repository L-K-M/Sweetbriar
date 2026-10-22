// swift-tools-version: 6.0

// The generated core bindings as a SwiftPM package (spec/08
// "Platform"). `SweetbriarCoreFFI` is the XCFramework `xtask ffi`
// builds; `SweetbriarCore` is the generated Swift surface on top of
// it. `Sources/` and `Headers/` are generated. This manifest and `Tests/`
// are handwritten; the main Xcode scheme runs the SDK qualification bundle.

import PackageDescription

let package = Package(
    name: "SweetbriarCore",
    platforms: [.macOS("26.0")],
    products: [
        .library(name: "SweetbriarCore", targets: ["SweetbriarCore"])
    ],
    targets: [
        .binaryTarget(
            name: "SweetbriarCoreFFI",
            path: "SweetbriarCoreFFI.xcframework"
        ),
        .target(
            name: "SweetbriarCore",
            dependencies: ["SweetbriarCoreFFI"],
            path: "Sources/SweetbriarCore",
            swiftSettings: [.swiftLanguageMode(.v6)],
            linkerSettings: [
                // rustc --print native-static-libs: the staticlib needs
                // libiconv at link time.
                .linkedLibrary("iconv")
            ]
        ),
    ]
)
