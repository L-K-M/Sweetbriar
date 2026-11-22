import Foundation
import SweetbriarBridge

/// The app's opt-in latency log (spec/08 "Latency log"): whether it is
/// on, stored in user defaults like List Implementation, and the log in
/// the cache root's `diagnostics/` once the roots are known. Recording
/// while off, or before the roots are known, does nothing.
@MainActor
final class LatencyRecorder {
    static let shared = LatencyRecorder()
    /// The user-defaults key Record Interaction Latency persists under.
    static let defaultsKey = "recordInteractionLatency"

    private let defaults: UserDefaults
    private(set) var log: LatencyLog?

    init(defaults: UserDefaults = .standard) {
        self.defaults = defaults
    }

    var isEnabled: Bool {
        get { defaults.bool(forKey: Self.defaultsKey) }
        set { defaults.set(newValue, forKey: Self.defaultsKey) }
    }

    /// Point the log at `cacheRoot`'s `diagnostics/`.
    func open(cacheRoot: URL) {
        log = LatencyLog(directory: cacheRoot.appendingPathComponent("diagnostics", isDirectory: true))
    }

    /// Record one span that ended now.
    func record(_ interaction: LatencyInteraction, milliseconds: Double, warm: Bool? = nil) {
        guard isEnabled, let log else { return }
        log.record(
            LatencyEntry(
                interaction: interaction, milliseconds: milliseconds,
                endedAtMs: Int64((Date().timeIntervalSince1970 * 1000).rounded()),
                appVersion: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
                    ?? "unknown",
                warm: warm))
    }

    /// The report's text, or why there is none.
    func reportText() -> String {
        guard let log else { return "The latency log opens with the archive; there is nothing to report yet." }
        let state = isEnabled ? "on" : "off"
        return "Recording is \(state). Log: \(log.fileURL.path)\n\n" + log.report().text
    }

    /// Debug → Clear Latency Log. The error, if the file couldn't be
    /// emptied.
    func clear() -> (any Error)? {
        do {
            try log?.clear()
            return nil
        } catch {
            return error
        }
    }
}
