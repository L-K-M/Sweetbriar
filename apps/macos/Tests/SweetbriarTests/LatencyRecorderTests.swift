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

    /// A span ends at its frame; one started while off, or whose view
    /// can't show it, records nothing.
    @Test func aSpanEndsAtTheFrameThatShowsIt() async throws {
        let fixture = try LatencyFixture()
        defer { fixture.cleanUp() }
        fixture.recorder.end(fixture.recorder.begin(.openCompose), shownIn: NSView())
        #expect(try await fixture.interactions(count: 1) == [.openCompose])

        fixture.recorder.begin(.search, replacing: 1 as UInt64)
        fixture.recorder.begin(.search, replacing: 2 as UInt64)
        fixture.recorder.end(.search, key: 1 as UInt64, shownIn: NSView())
        fixture.recorder.end(.search, key: 2 as UInt64, shownIn: NSView())
        #expect(try await fixture.interactions(count: 2) == [.openCompose, .search])

        fixture.recorder.isEnabled = false
        #expect(fixture.recorder.begin(.openCompose) == nil)
        #expect(await LatencyRecorder.nextFrame(of: NSView()) == false)
    }

    /// The launch is measured once, from the process's start; only a
    /// launch since the same boot as the last recorded one is warm.
    @Test func theLaunchIsMeasuredOnceAndWarmOnTheSameBoot() async throws {
        let fixture = try LatencyFixture()
        defer { fixture.cleanUp() }
        fixture.recorder.launchShown(in: NSView())
        fixture.recorder.launchShown(in: NSView())
        #expect(try await fixture.interactions(count: 1) == [.launch])
        let again = LatencyRecorder(defaults: fixture.defaults, frameWait: { _ in true })
        again.open(cacheRoot: fixture.root)
        again.launchShown(in: NSView())
        #expect(try await fixture.interactions(count: 2) == [.launch, .launch])
        let entries = try #require(fixture.recorder.log).read().entries
        #expect(entries.map(\.warm) == [false, true])
        #expect(entries.allSatisfy { $0.milliseconds > 0 })
    }

    /// The list's first rows end the launch, and a search's rows end the
    /// span its last keystroke started.
    @Test func theListEndsTheLaunchAndASearch() async throws {
        let fixture = try LatencyFixture()
        defer { fixture.cleanUp() }
        let config = CoreConfig(dataRoot: fixture.root.appendingPathComponent("data").path,
                                cacheRoot: fixture.root.appendingPathComponent("core-cache").path)
        let bridge = try await CoreBridge.open(
            config: config, snapshots: FrameBatcher<[Setting]>(clock: LatencyFrames()) { _ in })
        let sidebar = SidebarModel(source: bridge)
        let appearance = SharedAppearance()
        let controller = MessageListViewController(
            bridge: bridge, sidebar: sidebar, appearance: appearance.model(source: bridge),
            listSource: OneRowAnySpec(), clock: LatencyFrames(), latency: fixture.recorder)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 600, height: 300),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentViewController = controller
        defer { window.close() }

        controller.show(.mailbox(1))
        #expect(try await fixture.interactions(count: 1) == [.launch])
        controller.searchModelForBenchmark.updateText("subject:hello")
        #expect(try await fixture.interactions(count: 2) == [.launch, .search])
        await controller.close()
        await appearance.reset()
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

/// An enabled recorder over scratch defaults and a scratch cache root,
/// whose frames come at once.
@MainActor
final class LatencyFixture {
    let recorder: LatencyRecorder
    let defaults: UserDefaults
    let root: URL
    private let suite: String

    init() throws {
        suite = "sb-latency-\(UUID().uuidString)"
        defaults = try #require(UserDefaults(suiteName: suite))
        root = FileManager.default.temporaryDirectory.appendingPathComponent(suite, isDirectory: true)
        recorder = LatencyRecorder(defaults: defaults, frameWait: { _ in true })
        recorder.open(cacheRoot: root)
        recorder.isEnabled = true
    }

    func cleanUp() {
        defaults.removePersistentDomain(forName: suite)
        try? FileManager.default.removeItem(at: root)
    }

    /// The log's interactions once it holds `count` entries, or what it
    /// holds after five seconds.
    func interactions(count: Int) async throws -> [LatencyInteraction] {
        let deadline = ContinuousClock.now + .seconds(5)
        while ContinuousClock.now < deadline {
            let entries = try recorder.log?.read().entries ?? []
            if entries.count >= count { return entries.map(\.interaction) }
            try await Task.sleep(for: .milliseconds(10))
        }
        return try recorder.log?.read().entries.map(\.interaction) ?? []
    }
}

private struct LatencyFrames: FrameClock {
    func nextFrame() async { try? await Task.sleep(for: .milliseconds(1)) }
}

/// One row for whatever the list or a search asks for.
private actor OneRowAnySpec: ListWindowSource, WindowEventSource {
    func openWindow(spec: WindowSpec, anchor: SweetbriarBridge.Anchor, limit: UInt32) async throws -> ListWindow {
        let row = ListRow(occurrenceId: 1, messageId: 1, accountId: "account-1", mailboxId: 1,
                          sortDate: 0, sender: "Sender", subject: "Subject", snippet: "Snippet",
                          unread: false, flagged: false, hasAttachments: false, tags: [])
        return ListWindow(windowId: 1, generation: 0, rows: [row], before: nil, after: nil,
                          hasMoreAfter: false, hasMoreBefore: false, total: .exact(n: 1), search: nil)
    }
    func readWindow(id: UInt64, generation: UInt64, anchor: SweetbriarBridge.Anchor, limit: UInt32) async throws -> ListWindow {
        try await openWindow(spec: .debugSynthetic, anchor: anchor, limit: limit)
    }
    func windowIDs(id: UInt64) async throws -> WindowIDs { WindowIDs(generation: 0, ids: [1]) }
    func closeWindow(id: UInt64) async {}
    func windowEvents(id: UInt64) async throws -> any WindowEventSource { self }
    func next() async throws -> WindowEvent {
        try await Task.sleep(for: .seconds(3600))
        throw CancellationError()
    }
}
