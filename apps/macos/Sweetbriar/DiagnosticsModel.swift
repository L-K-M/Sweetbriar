import Foundation
import Observation
import SweetbriarBridge

/// The diagnostics preview's logic (spec/10 "Privacy", spec/07
/// `diagnostics_package`): build the package on demand and hold its
/// text for the sheet. The core writes no file, and neither does the
/// model; only the sheet's Save… writes, where the user chooses.
@MainActor
@Observable
final class DiagnosticsModel {
    /// The package's text, or nil until it loads.
    private(set) var text: String?
    /// True while the package is building; the sheet shows a spinner.
    private(set) var isLoading = false
    /// Why the package failed; the sheet shows it rather than an
    /// empty document.
    private(set) var error: String?

    private let source: any DiagnosticsSource

    init(source: any DiagnosticsSource) {
        self.source = source
    }

    /// The app's `CFBundleShortVersionString`, which the core does not
    /// know. A missing value still produces a document.
    static var appVersion: String {
        Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
            ?? "unknown"
    }

    /// The OS version string, also the shell's (spec/10).
    static var osVersion: String {
        ProcessInfo.processInfo.operatingSystemVersionString
    }

    /// Build the package. A failure clears the text and shows the
    /// error; it is never an empty success.
    func load() async {
        guard !isLoading else { return }
        isLoading = true
        error = nil
        do {
            text = try await source.diagnosticsPackage(
                appVersion: Self.appVersion, osVersion: Self.osVersion)
        } catch {
            text = nil
            self.error = error.localizedDescription
        }
        isLoading = false
    }
}
