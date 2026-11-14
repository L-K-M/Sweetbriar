import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private actor ComposeGate {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    func wait() async {
        if opened { return }
        await withCheckedContinuation { waiters.append($0) }
    }
    func open() {
        opened = true
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume() }
    }
}

/// Real native commands, with an optional held answer AFTER save committed.
/// This is receipt-delivery qualification, not physical disk-failure injection.
private actor ComposeTestSource: ComposeSource {
    let bridge: CoreBridge
    private var createGate: ComposeGate?
    private var answerGate: ComposeGate?
    private var failure: CoreFailure?
    private var parseHold: (text: String, gate: ComposeGate)?
    private(set) var parsed: [String] = []
    func holdParse(_ text: String, gate: ComposeGate) { parseHold = (text, gate) }
    private(set) var creates = 0
    private(set) var saves = 0
    private(set) var acknowledged = 0
    init(_ bridge: CoreBridge) { self.bridge = bridge }
    func holdCreate(_ gate: ComposeGate) { createGate = gate }
    func holdAnswer(_ gate: ComposeGate) { answerGate = gate }
    func failSave(_ error: CoreFailure?) { failure = error }
    func release() async {
        await createGate?.open()
        await answerGate?.open()
        if let parseHold { await parseHold.gate.open() }
        failure = nil
    }
    func createDraft(idempotencyKey: String, source: DraftSource) async throws -> DraftCreated {
        creates += 1
        await createGate?.wait()
        return try await bridge.createDraft(idempotencyKey: idempotencyKey, source: source)
    }
    func draft(id: String) async throws -> Draft { try await bridge.draft(id: id) }
    func parseDraftRecipients(text: String) async throws -> [DraftRecipient] {
        let answer = try await bridge.parseDraftRecipients(text: text)
        parsed.append(text)
        if let parseHold, parseHold.text.utf8.elementsEqual(text.utf8) { await parseHold.gate.wait() }
        return answer
    }
    func recipientCompletions(prefix: String, limit: UInt32) async throws -> [DraftRecipient] {
        try await bridge.recipientCompletions(prefix: prefix, limit: limit)
    }
    func saveDraft(
        idempotencyKey: String, draftId: String, baseRevisionId: String?,
        content: DraftContent
    ) async throws -> DraftSaved {
        saves += 1
        if let failure { throw failure }
        let saved = try await bridge.saveDraft(
            idempotencyKey: idempotencyKey, draftId: draftId,
            baseRevisionId: baseRevisionId, content: content)
        acknowledged += 1
        await answerGate?.wait()
        return saved
    }
    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt {
        try await bridge.discardDraft(idempotencyKey: idempotencyKey, draftId: draftId)
    }
}

private struct ComposeClock: FrameClock { func nextFrame() async { await Task.yield() } }

@MainActor
private func composeEventually(_ predicate: @MainActor () async -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !(await predicate()) {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

@MainActor
private final class ComposeFixture {
    let bridge: CoreBridge
    let source: ComposeTestSource
    let backups = SharedBackups(didRestore: {}, didClose: {})
    var controllers: [ComposeWindowController] = []
    var owner: ComposeWindows?
    var errors: [any Error] = []
    init(_ bridge: CoreBridge) {
        self.bridge = bridge
        source = ComposeTestSource(bridge)
    }
    func makeWindow(id: String? = nil, canWrite: @escaping @MainActor () -> Bool = { true }) async throws
        -> ComposeWindowController
    {
        let draftId: String
        if let id {
            draftId = id
        } else {
            draftId = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new).draftId
        }
        let controller = ComposeWindowController(source: source, draftId: draftId, canWrite: canWrite)
        controllers.append(controller)
        try #require(
            await composeEventually { controller.model.content != nil && controller.textView.isEditable })
        return controller
    }
    func makeSavedWindow(subject: String, body: String) async throws -> ComposeWindowController {
        let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
        let content = DraftContent(
            identityId: nil, format: .plain, plainFlowed: true,
            subject: subject, to: [], cc: [], bcc: [], bodySource: body, attachments: [])
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: created.draftId,
            baseRevisionId: nil, content: content)
        return try await makeWindow(id: created.draftId)
    }

    func makeOwner() -> ComposeWindows {
        let source = source
        let owner = ComposeWindows(
            backups: backups, openCore: { source },
            reportError: { [weak self] in
                self?.errors.append($0)
            })
        self.owner = owner
        return owner
    }
    func cleanUp() async -> Bool {
        await source.release()
        let all = controllers + (owner.map { Array($0.windows.values) } ?? [])
        for controller in all {
            if !(await controller.closeAndWait()) {
                // Explicitly abandon only synthetic test edits during cleanup.
                if !(await controller.closeWithoutSavingAndWait()) { return false }
            }
        }
        return await owner?.closeForQuit() != false
    }
}

@MainActor
private func withComposeFixture(
    reopened: (@MainActor (CoreBridge) async throws -> Void)? = nil,
    body: @MainActor (ComposeFixture) async throws -> Void
) async throws {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent(
        "sb-compose-\(UUID().uuidString)")
    let config = CoreConfig(
        dataRoot: root.appendingPathComponent("data").path,
        cacheRoot: root.appendingPathComponent("cache").path)
    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: FrameBatcher<[Setting]>(clock: ComposeClock()) { _ in })
    let fixture = ComposeFixture(bridge)
    let result: Result<Void, any Error>
    do {
        let model = try #require(fixture.backups.model(source: bridge))
        await model.refresh()
        try #require(await composeEventually { fixture.backups.canWrite })
        try await body(fixture)
        result = .success(())
    } catch { result = .failure(error) }
    let closed = await fixture.cleanUp()
    #expect(closed, "Retain failed fixture roots instead of deleting evidence: \(root.path)")
    await fixture.backups.cancelAndWait()
    await bridge.shutdown()
    if closed, let reopened {
        let second = try await CoreBridge.open(
            config: config,
            snapshots: FrameBatcher<[Setting]>(clock: ComposeClock()) { _ in })
        do { try await reopened(second) } catch {
            await second.shutdown()
            throw error
        }
        await second.shutdown()
    }
    if closed { try FileManager.default.removeItem(at: root) }
    try result.get()
}

@MainActor
private func editCompose(_ controller: ComposeWindowController, subject: String, body: String) {
    controller.subjectField.stringValue = subject
    controller.textView.string = body
    controller.textDidChange(Notification(name: NSText.didChangeNotification, object: controller.textView))
}

@MainActor
@discardableResult
private func editRecipient(_ field: RecipientField, in controller: ComposeWindowController, text: String)
    throws -> NSTextView
{
    controller.showWindow(nil)
    // Starting a fresh field-editing session resets AppKit's typing undo.
    // Replacements in an already-active editor must use that same session.
    if field.control.currentEditor() == nil { field.control.selectText(nil) }
    let editor = try #require(field.control.currentEditor() as? NSTextView)
    editor.insertText(text, replacementRange: NSRange(location: 0, length: editor.string.utf16.count))
    return editor
}

@MainActor
private final class ComposeCompletion {
    var started = false
    var finished = false
}

@MainActor
private func firstComposeView<T: NSView>(
    _ type: T.Type, in view: NSView, matching: (T) -> Bool = { _ in true }
) -> T? {
    if let result = view as? T, matching(result) { return result }
    for child in view.subviews {
        if let result = firstComposeView(type, in: child, matching: matching) { return result }
    }
    return nil
}

@Suite(.serialized, .timeLimit(.minutes(1))) @MainActor
struct ComposeWindowTests {
    @Test func quietRecipientValidationKeepsFocusAndSavesCompleteNamedTokens() async throws {
        try await withComposeFixture { f in
            let c = try await f.makeWindow()
            let editor = try editRecipient(
                c.toRecipients, in: c,
                text: "\"Doe, Alex\" <User@EXAMPLE.test>; 東京 <other@example.test>")
            #expect(c.toRecipients.isPending && c.model.isDirty)
            try #require(await composeEventually { c.statusLabel.stringValue == "Saved" })
            #expect(c.toRecipients.control.currentEditor() === editor)
            #expect((c.toRecipients.control.objectValue as? [NSObject])?.count == 2)
            let revision = try #require(try await f.bridge.draft(id: c.model.draftId).revision)
            #expect(
                revision.to == [
                    DraftRecipient(address: "User@EXAMPLE.test", name: "Doe, Alex"),
                    DraftRecipient(address: "other@example.test", name: "東京"),
                ])
            #expect(await f.source.saves == 1)
        }
    }

    @Test func allRecipientFieldsSurviveFormatChangesAndNativeTokenRemoval() async throws {
        try await withComposeFixture { f in
            let c = try await f.makeWindow()
            try editRecipient(c.toRecipients, in: c, text: "To Name <to@example.test>")
            try editRecipient(c.ccRecipients, in: c, text: "\"Cc, Name\" <cc@example.test>")
            try editRecipient(c.bccRecipients, in: c, text: "Bcc Name <Bcc@EXAMPLE.test>")
            editCompose(c, subject: "Named recipients", body: "**Source**")
            let view = try #require(c.window?.contentView)
            let format = try #require(firstComposeView(NSSegmentedControl.self, in: view))
            format.selectedSegment = 1
            let action = try #require(format.action)
            #expect(NSApp.sendAction(action, to: format.target, from: format))
            c.saveNow()
            try #require(await composeEventually { c.statusLabel.stringValue == "Saved" })
            let first = try #require(try await f.bridge.draft(id: c.model.draftId).revision)
            #expect(
                first.to.first?.name == "To Name" && first.cc.first?.name == "Cc, Name"
                    && first.bcc.first?.name == "Bcc Name")
            #expect(
                first.format == .markdown && first.plainFlowed && first.identityId == nil
                    && first.attachments.isEmpty)
            try editRecipient(c.ccRecipients, in: c, text: "")
            #expect(await c.closeAndWait())
            let final = try #require(try await f.bridge.draft(id: c.model.draftId).revision)
            #expect(final.cc.isEmpty && final.to == first.to && final.bcc == first.bcc)
            #expect(
                final.bodySource == "**Source**" && final.subject == "Named recipients"
                    && final.format == .markdown)
        }
    }

    @Test func anInvalidMemberRefusesCloseWithoutDroppingTheValidOne() async throws {
        try await withComposeFixture { f in
            let c = try await f.makeWindow()
            try editRecipient(c.toRecipients, in: c, text: "valid@example.test,broken")
            editCompose(c, subject: "Kept subject", body: "Kept body")
            #expect(!(await c.closeAndWait()))
            #expect(c.toRecipients.isPending && c.toRecipients.problem != nil)
            #expect(c.toRecipients.control.stringValue.contains("broken"))
            #expect(c.model.isDirty && c.textView.isEditable && c.textView.string == "Kept body")
            #expect(try await f.bridge.draft(id: c.model.draftId).revision == nil)
            #expect(await f.source.saves == 0)
            try editRecipient(c.toRecipients, in: c, text: "\"Fixed, Name\" <valid@example.test>")
            #expect(await c.closeAndWait())
            #expect(try await f.bridge.draft(id: c.model.draftId).revision?.to.first?.name == "Fixed, Name")
        }
    }

    @Test func lateAcknowledgmentDoesNotOverwriteNewRawRecipientInputOrClaimItSaved() async throws {
        try await withComposeFixture { f in
            let c = try await f.makeWindow()
            let gate = ComposeGate()
            await f.source.holdAnswer(gate)
            editCompose(c, subject: "Acknowledged", body: "Previous saved text")
            c.saveNow()
            try #require(await composeEventually { await f.source.acknowledged == 1 })
            try editRecipient(c.toRecipients, in: c, text: "valid@example.test,broken")
            await gate.open()
            try #require(await composeEventually { !c.model.isSaving })
            #expect(c.model.isDirty && c.toRecipients.isPending)
            #expect(c.statusLabel.stringValue != "Saved")
            #expect(c.toRecipients.control.stringValue.contains("broken"))
            #expect(!(await c.closeAndWait()))
            #expect(await c.closeWithoutSavingAndWait())
            let revision = try #require(try await f.bridge.draft(id: c.model.draftId).revision)
            #expect(revision.to.isEmpty && revision.bodySource == "Previous saved text")
            #expect(await f.source.saves == 1)
        }
    }

    @Test func staleParsingPreservesLaterRecipientsBodyAndSubject() async throws {
        try await withComposeFixture { f in
            let c = try await f.makeWindow()
            let gate = ComposeGate()
            let old = "Old <old@example.test>"
            await f.source.holdParse(old, gate: gate)
            try editRecipient(c.toRecipients, in: c, text: old)
            let saving = Task { await c.saveAndWait() }
            try #require(await composeEventually { await f.source.parsed.contains(old) })
            try editRecipient(c.toRecipients, in: c, text: "New <New@EXAMPLE.test>")
            editCompose(c, subject: "Later subject", body: "Later body")
            await gate.open()
            #expect(!(await saving.value))
            #expect(await c.closeAndWait())
            let revision = try #require(try await f.bridge.draft(id: c.model.draftId).revision)
            #expect(revision.to == [DraftRecipient(address: "New@EXAMPLE.test", name: "New")])
            #expect(revision.subject == "Later subject" && revision.bodySource == "Later body")
        }
    }

    @Test func abandonmentDrainsBegunParsingWithoutStartingANativeSave() async throws {
        try await withComposeFixture { f in
            let c = try await f.makeWindow()
            let gate = ComposeGate()
            let text = "Abandoned <abandoned@example.test>"
            await f.source.holdParse(text, gate: gate)
            try editRecipient(c.toRecipients, in: c, text: text)
            c.saveNow()
            try #require(await composeEventually { await f.source.parsed.contains(text) })
            let closing = Task { await c.closeWithoutSavingAndWait() }
            try #require(await composeEventually { c.isClosing && !c.toRecipients.control.isEditable })
            closing.cancel()
            #expect(await f.source.saves == 0)
            await gate.open()
            #expect(await closing.value)
            #expect(try await f.bridge.draft(id: c.model.draftId).revision == nil)
            #expect(await f.source.saves == 0)
        }
    }

    @Test func recipientTokenizationDoesNotDestroyNativeTypingUndo() async throws {
        try await withComposeFixture { f in
            let c = try await f.makeWindow()
            let editor = try editRecipient(c.toRecipients, in: c, text: "undo@example.test")
            try #require(await composeEventually { c.statusLabel.stringValue == "Saved" })
            let undo = try #require(editor.undoManager)
            #expect(undo.canUndo)
            if undo.canUndo { undo.undo() }
            #expect(await c.saveAndWait())
            #expect(try await f.bridge.draft(id: c.model.draftId).revision?.to.isEmpty == true)
            #expect(undo.canRedo)
            if undo.canRedo { undo.redo() }
            #expect(await c.saveAndWait())
            #expect(
                try await f.bridge.draft(id: c.model.draftId).revision?.to.first?.address
                    == "undo@example.test")
            #expect(undo.canUndo)
            if undo.canUndo { undo.undo() }
            #expect(await c.closeAndWait())
            #expect(try await f.bridge.draft(id: c.model.draftId).revision?.to.isEmpty == true)
        }
    }

    @Test func validatedRecipientEditsPreserveNativeWholeSessionUndoAndRedo() async throws {
        try await withComposeFixture { f in
            let c = try await f.makeWindow()
            let editor = try editRecipient(c.toRecipients, in: c, text: "first@example.test")
            try #require(await composeEventually { c.statusLabel.stringValue == "Saved" })
            editor.setSelectedRange(NSRange(location: 0, length: editor.string.utf16.count))
            editor.insertText("second@example.test", replacementRange: editor.selectedRange())
            #expect(await c.saveAndWait())
            #expect(
                try await f.bridge.draft(id: c.model.draftId).revision?.to.first?.address
                    == "second@example.test")
            let undo = try #require(editor.undoManager)
            // The independent unchanged-control characterization establishes
            // AppKit's whole-editing-session undo, not one step per autosave.
            #expect(undo.canUndo)
            if undo.canUndo { undo.undo() }
            #expect(await c.saveAndWait())
            #expect(try await f.bridge.draft(id: c.model.draftId).revision?.to.isEmpty == true)
            #expect(undo.canRedo)
            if undo.canRedo { undo.redo() }
            #expect(await c.closeAndWait())
            #expect(
                try await f.bridge.draft(id: c.model.draftId).revision?.to.first?.address
                    == "second@example.test")
        }
    }

    @Test func nativeRecipientClipboardFormattingRoundTripsNamesAndAddressBytes() async throws {
        try await withComposeFixture { f in
            let field = RecipientField(label: "To", source: f.bridge)
            let expected = [
                DraftRecipient(address: "User@EXAMPLE.test", name: "Doe, \"A\\B\" 東京"),
                DraftRecipient(address: "other@example.test", name: nil),
            ]
            field.load(expected)
            let objects = try #require(field.control.objectValue as? [Any])
            let pasteboard = NSPasteboard.withUniqueName()
            defer { pasteboard.clearContents() }
            #expect(field.tokenField(field.control, writeRepresentedObjects: objects, to: pasteboard))
            let text = try #require(pasteboard.string(forType: .string))
            #expect(RecipientField.sameBytes(try await f.bridge.parseDraftRecipients(text: text), expected))
            #expect(field.tokenField(field.control, readFrom: pasteboard)?.count == 1)
            await field.stopAndWait()
        }
    }

    @Test func normalizationOnlyEditsReachANewAcknowledgedRevision() async throws {
        try await withComposeFixture { f in
            let original = "Caf\u{e9}"
            let edited = "Cafe\u{301}"
            #expect(original == edited && !original.utf8.elementsEqual(edited.utf8))
            let controller = try await f.makeSavedWindow(subject: original, body: original)
            let base = controller.model.baseRevisionId
            editCompose(controller, subject: edited, body: edited)
            #expect(controller.textView.string.utf8.elementsEqual(edited.utf8))
            #expect(controller.model.isDirty, "canonically equivalent bytes are still an edit")
            #expect(controller.model.content?.subject.utf8.elementsEqual(edited.utf8) == true)
            #expect(controller.model.content?.bodySource.utf8.elementsEqual(edited.utf8) == true)
            #expect(await controller.closeAndWait())
            let revision = try #require(try await f.bridge.draft(id: controller.model.draftId).revision)
            #expect(revision.revisionId != base)
            #expect(revision.subject.utf8.elementsEqual(edited.utf8))
            #expect(revision.bodySource.utf8.elementsEqual(edited.utf8))
        }
    }

    @Test func cleanReloadPublishesExactCanonicallyEquivalentSourceBytes() async throws {
        try await withComposeFixture { f in
            let original = "Caf\u{e9}"
            let replacement = "Cafe\u{301}"
            let controller = try await f.makeSavedWindow(subject: original, body: original)
            var content = try #require(controller.model.content)
            content.subject = replacement
            content.bodySource = replacement
            let saved = try await f.bridge.saveDraft(
                idempotencyKey: UUID().uuidString,
                draftId: controller.model.draftId, baseRevisionId: controller.model.baseRevisionId,
                content: content)
            await controller.model.reload()
            try #require(controller.model.baseRevisionId == saved.revisionId && !controller.model.isDirty)
            try #require(
                await composeEventually {
                    controller.subjectField.stringValue.utf8.elementsEqual(replacement.utf8)
                        && controller.textView.string.utf8.elementsEqual(replacement.utf8)
                }, "a clean reload must publish the exact stored source, not its old equivalent")
        }
    }

    @Test func nativeFieldAndTextViewDelegatesReachTheRevisionModel() async throws {
        try await withComposeFixture { f in
            let controller = try await f.makeWindow()
            controller.showWindow(nil)
            let window = try #require(controller.window)
            try #require(window.makeFirstResponder(controller.subjectField))
            let fieldEditor = try #require(controller.subjectField.currentEditor() as? NSTextView)
            fieldEditor.insertText("Native subject", replacementRange: NSRange(location: 0, length: 0))
            #expect(controller.model.content?.subject == "Native subject")
            try #require(window.makeFirstResponder(controller.textView))
            controller.textView.insertText("Native body", replacementRange: NSRange(location: 0, length: 0))
            #expect(controller.model.content?.bodySource == "Native body" && controller.model.isDirty)
            #expect(controller.textView.isOnTextKit2)
            #expect(await controller.closeAndWait())
            let revision = try #require(try await f.bridge.draft(id: controller.model.draftId).revision)
            #expect(revision.subject == "Native subject" && revision.bodySource == "Native body")
        }
    }

    @Test func disabledEditorCannotFormatFrozenText() {
        let editor = ComposerTextView()
        editor.string = "Frozen"
        editor.mode = .markdown
        editor.setSelectedRange(NSRange(location: 0, length: 6))
        editor.isEditable = false
        let bold = NSMenuItem(
            title: "Bold", action: #selector(ComposerTextView.toggleBoldMarkdown(_:)), keyEquivalent: "b")
        #expect(!editor.validateMenuItem(bold))
        editor.toggleBoldMarkdown(nil)
        #expect(editor.string == "Frozen")
        #expect(editor.isOnTextKit2)
    }

    @Test func closeWaitsForNativeReceiptAndPreservesOtherFields() async throws {
        try await withComposeFixture { f in
            let created = try await f.bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
            let object = try await f.bridge.publishDraftAttachment(bytes: Data("attachment".utf8))
            let content = DraftContent(
                identityId: nil, format: .plain, plainFlowed: false, subject: "Original",
                to: [DraftRecipient(address: "to@example.test", name: "Recipient")],
                cc: [DraftRecipient(address: "cc@example.test", name: nil)],
                bcc: [DraftRecipient(address: "bcc@example.test", name: "Hidden")], bodySource: "Old body",
                attachments: [.new(object: object, filename: "note.txt", mediaType: "text/plain")])
            _ = try await f.bridge.saveDraft(
                idempotencyKey: UUID().uuidString, draftId: created.draftId,
                baseRevisionId: nil, content: content)
            let controller = try await f.makeWindow(id: created.draftId)
            let gate = ComposeGate()
            await f.source.holdAnswer(gate)
            editCompose(controller, subject: "Edited subject", body: "# Café\nمرحبا\n")
            let completion = ComposeCompletion()
            let close = Task {
                completion.started = true
                let result = await controller.closeAndWait()
                completion.finished = true
                return result
            }
            try #require(await composeEventually { await f.source.acknowledged == 1 })
            #expect(
                controller.isClosing && !controller.textView.isEditable && !controller.subjectField.isEditable
            )
            #expect(controller.statusLabel.stringValue == "Saving…" && !completion.finished)
            let native = try #require(try await f.bridge.draft(id: created.draftId).revision)
            #expect(native.subject == "Edited subject" && native.bodySource == "# Café\nمرحبا\n")
            #expect(native.to == content.to && native.cc == content.cc && native.bcc == content.bcc)
            #expect(native.identityId == nil && native.accountId == nil && !native.plainFlowed)
            #expect(native.attachments.first?.objectSha256 == object.objectSha256)
            close.cancel()
            #expect(!completion.finished)
            await gate.open()
            #expect(await close.value)
            #expect(!controller.model.isDirty && controller.model.baseRevisionId == native.revisionId)
            #expect(controller.textView.isOnTextKit2)
        }
    }

    @Test func failedFlushRefusesCloseAndKeepsTextForExplicitRetry() async throws {
        try await withComposeFixture { f in
            let controller = try await f.makeWindow()
            await f.source.failSave(.unavailable(reason: "synthetic save refusal"))
            editCompose(controller, subject: "Keep me", body: "Unacknowledged text")
            #expect(!(await controller.closeAndWait()))
            #expect(!controller.isClosing && controller.textView.isEditable)
            #expect(controller.model.isDirty && controller.textView.string == "Unacknowledged text")
            #expect(controller.statusLabel.stringValue.contains("synthetic save refusal"))
            #expect(try await f.bridge.draft(id: controller.model.draftId).revision == nil)
            await f.source.failSave(nil)
            #expect(await controller.closeAndWait())
            #expect(
                try await f.bridge.draft(id: controller.model.draftId).revision?.bodySource
                    == "Unacknowledged text")
        }
    }

    @Test func conflictRefusesCloseWithoutRetryingOrReplacingEitherContent() async throws {
        try await withComposeFixture { f in
            let controller = try await f.makeWindow()
            editCompose(controller, subject: "Losing window", body: "Keep this unsaved text")
            let winner = DraftContent(
                identityId: nil, format: .plain, plainFlowed: true, subject: "Winner",
                to: [], cc: [], bcc: [], bodySource: "Other window's revision", attachments: [])
            _ = try await f.bridge.saveDraft(
                idempotencyKey: UUID().uuidString, draftId: controller.model.draftId,
                baseRevisionId: nil, content: winner)
            #expect(!(await controller.closeAndWait()))
            #expect(!(await controller.closeAndWait()))
            #expect(controller.model.conflict != nil && controller.model.isDirty)
            #expect(controller.textView.string == "Keep this unsaved text")
            #expect(
                try await f.bridge.draft(id: controller.model.draftId).revision?.bodySource
                    == winner.bodySource)
            #expect(await f.source.saves == 1)
        }
    }

    @Test func pendingCreateBlocksRestoreAndCancelledQuitStillDrainsIt() async throws {
        try await withComposeFixture { f in
            let gate = ComposeGate()
            await f.source.holdCreate(gate)
            let owner = f.makeOwner()
            owner.newMessage()
            try #require(await composeEventually { await f.source.creates == 1 })
            #expect(owner.windows.isEmpty && f.backups.restoreBlockedReason?.contains("compose") == true)
            let completion = ComposeCompletion()
            let quit = Task {
                completion.started = true
                let result = await owner.closeForQuit()
                completion.finished = true
                return result
            }
            try #require(await composeEventually { completion.started })
            quit.cancel()
            #expect(!completion.finished)
            await gate.open()
            #expect(await quit.value)
            #expect(owner.windows.isEmpty && f.backups.restoreBlockedReason == nil && f.errors.isEmpty)
            let drafts = try await f.bridge.drafts()
            let row = try #require(drafts.first)
            #expect(drafts.count == 1)
            #expect(try await f.bridge.draft(id: row.draftId).revision?.seq == 1)
        }
    }

    @Test func failedQuitKeepsAdmissionUntilAnExplicitSuccessfulRetry() async throws {
        try await withComposeFixture { f in
            let owner = f.makeOwner()
            owner.newMessage()
            try #require(await composeEventually { owner.windows.values.first?.model.content != nil })
            let controller = try #require(owner.windows.values.first)
            editCompose(controller, subject: "Quit refusal", body: "Still here")
            await f.source.failSave(.unavailable(reason: "synthetic quit refusal"))
            #expect(!(await owner.closeForQuit()))
            #expect(owner.windows.count == 1 && f.backups.restoreBlockedReason != nil)
            #expect(controller.model.isDirty && controller.textView.isEditable)
            await f.source.failSave(nil)
            #expect(await owner.closeForQuit())
            #expect(owner.windows.isEmpty && f.backups.restoreBlockedReason == nil)
            #expect(
                try await f.bridge.draft(id: controller.model.draftId).revision?.bodySource == "Still here")
        }
    }

    @Test func aClosedNativeEditorReopensItsSavedRevisionWithoutASendingAccount() async throws {
        var draftId: String?
        var revisionId: String?
        try await withComposeFixture(
            reopened: { bridge in
                let id = try #require(draftId)
                let revision = try #require(try await bridge.draft(id: id).revision)
                #expect(revision.revisionId == revisionId && revision.subject == "Reopen me")
                #expect(
                    revision.bodySource == "**Saved source**" && revision.identityId == nil
                        && revision.accountId == nil)
                #expect(revision.format == .markdown)
                #expect(try await bridge.accounts().allSatisfy { $0.kind == .local })
            },
            body: { f in
                let controller = try await f.makeWindow()
                editCompose(controller, subject: "Reopen me", body: "**Saved source**")
                let content = try #require(controller.window?.contentView)
                let format = try #require(firstComposeView(NSSegmentedControl.self, in: content))
                format.selectedSegment = 1
                let action = try #require(format.action)
                #expect(NSApp.sendAction(action, to: format.target, from: format))
                #expect(controller.textView.mode == .markdown && controller.textView.isOnTextKit2)
                #expect(await controller.closeAndWait())
                draftId = controller.model.draftId
                revisionId = controller.model.baseRevisionId
                #expect(revisionId != nil)
            })
    }

    @Test func restoreRefusesAnOpenComposerAndSucceedsAfterItsReceiptAwareClose() async throws {
        try await withComposeFixture { f in
            let model = try #require(f.backups.model(source: f.bridge))
            model.backUpNow()
            try #require(await composeEventually { !model.isBusy })
            guard case .backup(let generation, .complete) = model.lastResult else {
                throw CoreFailure.internalError(detail: "synthetic backup did not complete")
            }
            let owner = f.makeOwner()
            owner.newMessage()
            try #require(await composeEventually { owner.windows.values.first?.model.content != nil })
            let controller = try #require(owner.windows.values.first)
            editCompose(controller, subject: "After backup", body: "Acknowledge before closing")
            model.selectedGenerationId = generation
            model.requestRestore()
            #expect(model.confirmation != nil && !f.backups.confirmRestore())
            #expect(!model.isBusy && model.archiveRevision == 0)
            #expect(await controller.closeAndWait())
            #expect(
                try await f.bridge.draft(id: controller.model.draftId).revision?.bodySource
                    == "Acknowledge before closing")
            #expect(f.backups.confirmRestore())
            try #require(await composeEventually { !model.isBusy && model.archiveRevision == 1 })
            #expect(
                try await f.bridge.drafts().isEmpty, "The explicit restore replaced the post-backup draft")
            #expect(await f.source.saves == 1, "No closed editor saved into the replacement archive")
        }
    }

    @Test func explicitAbandonReleasesRecoveryWithoutChangingTheSavedRevision() async throws {
        try await withComposeFixture { f in
            let owner = f.makeOwner()
            owner.newMessage()
            try #require(await composeEventually { owner.windows.values.first?.model.content != nil })
            let controller = try #require(owner.windows.values.first)
            editCompose(controller, subject: "Saved", body: "Last saved revision")
            controller.saveNow()
            try #require(
                await composeEventually {
                    controller.model.baseRevisionId != nil && !controller.model.isSaving
                })
            let saved = controller.model.baseRevisionId
            editCompose(controller, subject: "Unsaved", body: "Explicitly abandon only this edit")
            await f.source.failSave(.unavailable(reason: "synthetic read-only refusal"))
            #expect(!(await owner.closeForQuit()))
            #expect(f.backups.restoreBlockedReason != nil)
            #expect(await controller.closeWithoutSavingAndWait())
            #expect(owner.windows.isEmpty && f.backups.restoreBlockedReason == nil)
            let native = try #require(try await f.bridge.draft(id: controller.model.draftId).revision)
            #expect(native.revisionId == saved && native.bodySource == "Last saved revision")
            #expect(await f.source.saves == 2, "Explicit abandonment must not submit another write")
        }
    }

    @Test func explicitAbandonAwaitsABegunSaveAndDoesNotSaveLaterTyping() async throws {
        try await withComposeFixture { f in
            let owner = f.makeOwner()
            owner.newMessage()
            try #require(await composeEventually { owner.windows.values.first?.model.content != nil })
            let controller = try #require(owner.windows.values.first)
            let gate = ComposeGate()
            await f.source.holdAnswer(gate)
            editCompose(controller, subject: "Begun", body: "Already committed")
            controller.saveNow()
            try #require(await composeEventually { await f.source.acknowledged == 1 })
            editCompose(controller, subject: "Later", body: "Unacknowledged typing")
            let completion = ComposeCompletion()
            let close = Task {
                completion.started = true
                let result = await controller.closeWithoutSavingAndWait()
                completion.finished = true
                return result
            }
            try #require(await composeEventually { controller.isClosing })
            close.cancel()
            #expect(!completion.finished && f.backups.restoreBlockedReason != nil)
            await gate.open()
            #expect(await close.value)
            #expect(owner.windows.isEmpty && f.backups.restoreBlockedReason == nil)
            #expect(
                try await f.bridge.draft(id: controller.model.draftId).revision?.bodySource
                    == "Already committed")
            #expect(await f.source.saves == 1)
        }
    }

    @Test func currentWriteRefusalStillAllowsQueriesAndExplicitAbandonment() async throws {
        try await withComposeFixture { f in
            let controller = try await f.makeWindow(canWrite: { false })
            editCompose(controller, subject: "Unavailable", body: "Keep locally until an explicit decision")
            #expect(!(await controller.closeAndWait()))
            #expect(controller.model.isDirty && controller.textView.isEditable)
            #expect(controller.statusLabel.stringValue.contains("Archive writes are unavailable"))
            #expect(
                await f.source.saves == 0, "The UI guard refuses an unbegun write before reaching the service"
            )
            #expect(try await f.bridge.draft(id: controller.model.draftId).revision == nil)
            #expect(await controller.closeWithoutSavingAndWait())
        }
    }

    @Test func nativeAbandonPromptKeepsEditingByDefaultAndRequiresDestructiveConfirmation() async throws {
        try await withComposeFixture { f in
            let owner = f.makeOwner()
            owner.newMessage()
            try #require(await composeEventually { owner.windows.values.first?.model.content != nil })
            let controller = try #require(owner.windows.values.first)
            editCompose(controller, subject: "Decision", body: "Unsaved decision text")
            let parent = try #require(controller.window)
            let content = try #require(parent.contentView)
            let action = try #require(
                firstComposeView(
                    NSButton.self, in: content,
                    matching: {
                        $0.title == "Close Without Saving…"
                    }))
            action.performClick(nil)
            try #require(await composeEventually { parent.attachedSheet != nil })
            let firstSheet = try #require(parent.attachedSheet?.contentView)
            let keep = try #require(
                firstComposeView(
                    NSButton.self, in: firstSheet,
                    matching: {
                        $0.title == "Keep Editing"
                    }))
            #expect(!keep.hasDestructiveAction)
            #expect(parent.attachedSheet?.defaultButtonCell === keep.cell)
            controller.windowDidResignKey(Notification(name: NSWindow.didResignKeyNotification))
            keep.performClick(nil)
            try #require(await composeEventually { parent.attachedSheet == nil })
            #expect(owner.windows.count == 1 && controller.model.isDirty)
            #expect(controller.textView.string == "Unsaved decision text")
            action.performClick(nil)
            try #require(await composeEventually { parent.attachedSheet != nil })
            let secondSheet = try #require(parent.attachedSheet?.contentView)
            let abandon = try #require(
                firstComposeView(
                    NSButton.self, in: secondSheet,
                    matching: {
                        $0.title == "Close Without Saving"
                    }))
            #expect(abandon.hasDestructiveAction)
            abandon.performClick(nil)
            try #require(await composeEventually { owner.windows.isEmpty })
            #expect(f.backups.restoreBlockedReason == nil && parent.attachedSheet == nil)
            #expect(await f.source.saves == 0)
            #expect(try await f.bridge.draft(id: controller.model.draftId).revision == nil)
        }
    }

    @Test func normalCloseCancelsItsOwnAbandonPromptAndFlushesInstead() async throws {
        try await withComposeFixture { f in
            let controller = try await f.makeWindow()
            controller.showWindow(nil)
            editCompose(controller, subject: "Normal close", body: "Do not abandon this text")
            let parent = try #require(controller.window)
            let content = try #require(parent.contentView)
            let action = try #require(
                firstComposeView(
                    NSButton.self, in: content,
                    matching: {
                        $0.title == "Close Without Saving…"
                    }))
            action.performClick(nil)
            try #require(await composeEventually { parent.attachedSheet != nil })
            #expect(await controller.closeAndWait())
            #expect(parent.attachedSheet == nil)
            #expect(
                try await f.bridge.draft(id: controller.model.draftId).revision?.bodySource
                    == "Do not abandon this text")
            #expect(await f.source.saves == 1)
        }
    }

    @Test func explicitAbandonDoesNotSaveAnUnbegunEdit() async throws {
        try await withComposeFixture { f in
            let controller = try await f.makeWindow()
            editCompose(controller, subject: "Never saved", body: "Abandon this edit")
            #expect(await controller.closeWithoutSavingAndWait())
            #expect(await f.source.saves == 0)
            #expect(try await f.bridge.draft(id: controller.model.draftId).revision == nil)
        }
    }

    @Test func failedQuitShowsAWindowWhoseCreateFinishedDuringQuit() async throws {
        try await withComposeFixture { f in
            let gate = ComposeGate()
            await f.source.holdCreate(gate)
            await f.source.failSave(.unavailable(reason: "synthetic late-window refusal"))
            let owner = f.makeOwner()
            owner.newMessage()
            try #require(await composeEventually { await f.source.creates == 1 })
            let completion = ComposeCompletion()
            let quit = Task {
                completion.started = true
                return await owner.closeForQuit()
            }
            try #require(await composeEventually { completion.started })
            await gate.open()
            #expect(!(await quit.value))
            let controller = try #require(owner.windows.values.first)
            #expect(controller.window?.isVisible == true && controller.textView.isEditable)
            #expect(controller.statusLabel.stringValue.contains("synthetic late-window refusal"))
            #expect(f.backups.restoreBlockedReason != nil)
        }
    }

    @Test func blurFlushesTheDisplayedTextThroughTheNativeCore() async throws {
        try await withComposeFixture { f in
            let controller = try await f.makeWindow()
            editCompose(controller, subject: "Blur", body: "Saved on blur")
            controller.windowDidResignKey(Notification(name: NSWindow.didResignKeyNotification))
            try #require(
                await composeEventually {
                    !controller.model.isDirty && !controller.model.isSaving
                        && controller.model.baseRevisionId != nil
                        && controller.statusLabel.stringValue == "Saved"
                })
            #expect(controller.statusLabel.stringValue == "Saved")
            #expect(
                try await f.bridge.draft(id: controller.model.draftId).revision?.bodySource == "Saved on blur"
            )
        }
    }
}
