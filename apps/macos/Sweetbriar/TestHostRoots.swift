// A unit-test host's archive (spec/11): the app-hosted `SweetbriarTests`
// bundle runs inside the app, so a host given no roots would open the
// user's real archive, migrate it with a development build and write to
// it. Such a host opens a private pair of roots instead.

import Foundation
import SweetbriarBridge

enum TestHostRoots {
    /// The keys Xcode sets in an app that hosts a test bundle.
    static let markers = [
        "XCTestBundlePath", "XCTestConfigurationFilePath", "XCTestSessionIdentifier",
    ]

    /// Whether `environment` belongs to an app hosting a test bundle.
    static func isTestHost(
        _ environment: [String: String] = ProcessInfo.processInfo.environment
    ) -> Bool {
        markers.contains { environment[$0] != nil }
    }

    /// A fresh private pair of roots under `directory`.
    static func make(in directory: URL = FileManager.default.temporaryDirectory) -> CoreConfig {
        let root = directory.appendingPathComponent("SweetbriarTests-\(UUID().uuidString)", isDirectory: true)
        return CoreConfig(
            dataRoot: root.appendingPathComponent("data", isDirectory: true).path,
            cacheRoot: root.appendingPathComponent("cache", isDirectory: true).path)
    }

    /// The directory holding both of `roots`, which the host removes when it quits.
    static func parent(of roots: CoreConfig) -> URL {
        URL(fileURLWithPath: roots.dataRoot, isDirectory: true).deletingLastPathComponent()
    }
}
