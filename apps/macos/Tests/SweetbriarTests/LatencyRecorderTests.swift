import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

/// spec/08 "Latency log": off by default, toggled from the Debug menu,
/// and recording nothing while off.
@Suite
@MainActor
struct LatencyRecorderTests {
    private func scratch() throws -> (UserDefaults, URL, String) {
        let suite = "sb-latency-\(UUID().uuidString)"
        let defaults = try #require(UserDefaults(suiteName: suite))
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent(suite, isDirectory: true)
        return (defaults, root, suite)
    }

    @Test func recordingIsOffByDefaultAndWritesNothingWhileOff() throws {
        let (defaults, root, suite) = try scratch()
        defer {
            defaults.removePersistentDomain(forName: suite)
            try? FileManager.default.removeItem(at: root)
        }
        let recorder = LatencyRecorder(defaults: defaults)
        recorder.open(cacheRoot: root)
        #expect(!recorder.isEnabled)
        recorder.record(.search, milliseconds: 12)
        let log = try #require(recorder.log)
        #expect(!FileManager.default.fileExists(atPath: log.fileURL.path))
        #expect(log.fileURL.path.hasSuffix("/diagnostics/latency.jsonl"))

        recorder.isEnabled = true
        recorder.record(.search, milliseconds: 12)
        #expect(try log.read().entries.map(\.milliseconds) == [12])
        #expect(recorder.reportText().contains("Recording is on."))
        #expect(recorder.clear() == nil)
        #expect(try log.read().entries.isEmpty)
    }

    /// Before the roots are known there is no log, and the report says so.
    @Test func withoutTheRootsThereIsNothingToRecord() throws {
        let (defaults, _, suite) = try scratch()
        defer { defaults.removePersistentDomain(forName: suite) }
        let recorder = LatencyRecorder(defaults: defaults)
        recorder.isEnabled = true
        recorder.record(.search, milliseconds: 1)
        #expect(recorder.log == nil)
        #expect(recorder.reportText().contains("nothing to report yet"))
    }

    /// The Debug menu's three items, and the toggle's check following
    /// the stored choice.
    @Test func theDebugMenuTogglesRecording() throws {
        let (defaults, root, suite) = try scratch()
        defer {
            defaults.removePersistentDomain(forName: suite)
            try? FileManager.default.removeItem(at: root)
        }
        let recorder = LatencyRecorder(defaults: defaults)
        let oldWindowMenu = NSApp.windowsMenu
        defer { NSApp.windowsMenu = oldWindowMenu }
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {},
            latency: recorder
        )
        let debug = try #require(builder.makeMainMenu().items.compactMap(\.submenu).first { $0.title == "Debug" })
        let record = try #require(debug.items.first { $0.title == "Record Interaction Latency" })
        #expect(debug.items.contains { $0.title == "Latency Report…" })
        #expect(debug.items.contains { $0.title == "Clear Latency Log" })

        #expect(builder.validateMenuItem(record))
        #expect(record.state == .off)
        let action = try #require(record.action)
        NSApp.sendAction(action, to: record.target, from: record)
        #expect(recorder.isEnabled)
        #expect(builder.validateMenuItem(record))
        #expect(record.state == .on)
    }
}
