// Archive and delete in the latency log (spec/08 "Latency log"): the
// durable span ends at the receipt, the visible one at the first frame
// after the filed rows left the list, and only Archive and Move to
// Trash record them.

import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@Suite("filing latency", .serialized)
@MainActor
struct FilingLatencyTests {
    @Test func archiveAndTrashRecordTheirSpansAndTheRestNone() throws {
        let archive = try #require(FilingController.latency(of: .archive))
        #expect(archive.visible == .archiveVisible && archive.durable == .archiveDurable)
        let trash = try #require(FilingController.latency(of: .trash))
        #expect(trash.visible == .deleteVisible && trash.durable == .deleteDurable)
        let mailbox = Mailbox(
            mailboxId: 9, accountId: "account-1", name: "Travel", delimiter: nil, role: nil,
            kind: .folder, selectable: true, total: 0, unread: 0)
        #expect(FilingController.latency(of: .move(to: mailbox)) == nil)
        #expect(FilingController.latency(of: .addLabel(mailbox)) == nil)
        #expect(FilingController.latency(of: .removeLabel(mailbox)) == nil)
    }

    /// A receipt has nothing to show: its span ends when it comes.
    @Test func aReceiptEndsItsSpanAtOnce() async throws {
        let fixture = try LatencyFixture()
        defer { fixture.cleanUp() }
        fixture.recorder.finish(fixture.recorder.begin(.archiveDurable))
        #expect(try await fixture.interactions(count: 1) == [.archiveDurable])
    }

    /// The visible span waits while the row is listed and ends once a
    /// new generation took it out.
    @Test func aFiledRowEndsItsVisibleSpanOnceItLeavesTheList() async throws {
        let fixture = try LatencyFixture()
        defer { fixture.cleanUp() }
        let config = CoreConfig(
            dataRoot: fixture.root.appendingPathComponent("data").path,
            cacheRoot: fixture.root.appendingPathComponent("core-cache").path)
        let bridge = try await CoreBridge.open(
            config: config, snapshots: FrameBatcher<[Setting]>(clock: SoonFrames()) { _ in })
        let sidebar = SidebarModel(source: bridge)
        let appearance = SharedAppearance()
        let source = FilableRows(ids: [3, 2, 1])
        let controller = MessageListViewController(
            bridge: bridge, sidebar: sidebar, appearance: appearance.model(source: bridge),
            listSource: source, clock: SoonFrames(), latency: fixture.recorder)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 600, height: 300),
            styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentViewController = controller
        defer { window.close() }

        controller.show(.mailbox(1))
        #expect(try await fixture.interactions(count: 1) == [.launch])

        controller.endWhenGone(fixture.recorder.begin(.archiveVisible), ids: [2])
        try await Task.sleep(for: .milliseconds(200))
        #expect(try fixture.recorder.log?.read().entries.count == 1, "row 2 is still listed")

        await source.file([2])
        let model = try #require(controller.model)
        try await model.rebuild()
        #expect(try await fixture.interactions(count: 2) == [.launch, .archiveVisible])

        await controller.close()
        await appearance.reset()
    }
}

private struct SoonFrames: FrameClock {
    func nextFrame() async { try? await Task.sleep(for: .milliseconds(1)) }
}

/// A mailbox's rows, from which `file` takes some at a new generation.
private actor FilableRows: ListWindowSource, WindowEventSource {
    private var ids: [Int64]
    private var generation: UInt64 = 1

    init(ids: [Int64]) { self.ids = ids }

    func file(_ filed: Set<Int64>) {
        ids.removeAll { filed.contains($0) }
        generation += 1
    }

    private func page() -> ListWindow {
        let rows = ids.enumerated().map { rank, id in
            ListRow(
                occurrenceId: id, messageId: id, accountId: "account-1", mailboxId: 1,
                sortDate: -Int64(rank), sender: "Sender", subject: "Subject", snippet: "Snippet",
                unread: false, flagged: false, hasAttachments: false, tags: [])
        }
        return ListWindow(
            windowId: 1, generation: generation, rows: rows, before: nil, after: nil,
            hasMoreAfter: false, hasMoreBefore: false, total: .exact(n: UInt64(ids.count)),
            search: nil)
    }

    func openWindow(
        spec: WindowSpec, anchor: SweetbriarBridge.Anchor, limit: UInt32
    ) async throws -> ListWindow { page() }

    func readWindow(
        id: UInt64, generation: UInt64, anchor: SweetbriarBridge.Anchor, limit: UInt32
    ) async throws -> ListWindow { page() }

    func windowIDs(id: UInt64) async throws -> WindowIDs {
        WindowIDs(generation: generation, ids: ids)
    }

    func closeWindow(id: UInt64) async {}

    func windowEvents(id: UInt64) async throws -> any WindowEventSource { self }

    func next() async throws -> WindowEvent {
        try await Task.sleep(for: .seconds(3600))
        throw CancellationError()
    }
}
