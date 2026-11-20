// Diagnostics across the L1 boundary (spec/10 "Privacy", spec/07
// `diagnostics_package`): the package's text, which the shell
// previews and writes only where the user chooses. The core writes no
// file. App code never imports `SweetbriarCore`.

import Foundation

/// The diagnostics calls the shell needs (spec/07, spec/10).
/// `CoreBridge` adopts it; tests substitute a fake.
public protocol DiagnosticsSource: Sendable {
    /// The one UTF-8 JSON document, built from the shell's app and OS
    /// versions. A query: a fresh `CancelToken` per call.
    func diagnosticsPackage(appVersion: String, osVersion: String) async throws -> String
}
