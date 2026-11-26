import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
@testable import Sweetbriar

private actor AmbiguousImport: ImportJobSource {
    let mailbox = String(repeating: "Courrier 長い箱 ", count: 12)

    func startImport(paths: [String], target: ImportTarget) async throws -> ImportStarted {
        throw CoreFailure.unavailable(reason: "not used")
    }
    func confirmImport(jobId: String, variants: [VariantChoice]) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "not used")
    }
    func cancelJob(jobId: String) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "not used")
    }
    func importReport(jobId: String) async throws -> ImportReport? { nil }
    func importJob(id: String) async throws -> ImportJob {
        let source = ImportSource(
            ordinal: 1, path: "/tmp/mail.mbox", mailbox: mailbox, kind: .mbox,
            messages: 1, bytes: 1, sha256: "", needsVariant: true,
            variant: nil, messagesDone: 0)
        return ImportJob(
            id: id, state: .awaitingConfirmation, sources: [source], problems: [],
            problemCount: 0, totals: ImportTotals(
                processed: 0, imported: 0, duplicates: 0, rejected: 0,
                bytesStored: 0, readFromHeaders: 0, readDefault: 0, flagsIncomplete: 0),
            failure: nil, reportPath: nil)
    }
}

@MainActor
private func variantPopup(in view: NSView) -> NSPopUpButton? {
    if view.accessibilityIdentifier() == "importVariant-1" { return view as? NSPopUpButton }
    for child in view.subviews {
        if let found = variantPopup(in: child) { return found }
    }
    return nil
}

@Suite("Import variant layout")
@MainActor
struct ImportVariantLayoutTests {
    @Test func longMailboxKeepsItsVariantPickerVisibleAndNamed() async throws {
        let source = AmbiguousImport()
        let session = ImportSession(source: source, pollInterval: .milliseconds(10))
        session.attach(jobId: "synthetic")
        defer { session.detach() }
        let deadline = ContinuousClock.now + .seconds(5)
        while session.job == nil && ContinuousClock.now < deadline {
            try await Task.sleep(for: .milliseconds(10))
        }
        try #require(session.job != nil)
        let controller = ImportSheetController(session: session)
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 520, height: 600),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.contentView = controller.view
        controller.view.layoutSubtreeIfNeeded()
        let popup = try #require(variantPopup(in: controller.view))
        var ancestor = popup.superview
        while ancestor != nil && !(ancestor is NSClipView) { ancestor = ancestor?.superview }
        let clip = try #require(ancestor as? NSClipView)
        let frame = popup.convert(popup.bounds, to: clip)
        let mailbox = source.mailbox
        #expect(clip.bounds.width > 400)
        #expect(frame.width > 150)
        #expect(frame.minX >= clip.bounds.minX - 0.5)
        #expect(frame.maxX <= clip.bounds.maxX + 0.5)
        #expect(popup.accessibilityLabel()?.contains(mailbox) == true)
    }
}
#endif
