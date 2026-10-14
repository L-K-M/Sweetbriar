// swift-tools-version: 6.0

// The generated core bindings as a SwiftPM package (spec/08
// "Platform"). `SweetbriarCoreFFI` is the XCFramework `xtask ffi`
// builds; `SweetbriarCore` is the generated Swift surface on top of
// it. Everything in `Sources/` and `Headers/` is generated — only this
// manifest is handwritten.

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
