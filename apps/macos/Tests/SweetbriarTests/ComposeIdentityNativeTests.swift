import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private actor NativeIdentityGate {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    func wait() async { if !opened { await withCheckedContinuation { waiters.append($0) } } }
    func open() {
        opened = true
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume() }
    }
}

private func nativeIdentity(_ id: String, sendable: IdentitySendable = .validated) -> ComposeIdentity {
    ComposeIdentity(
        identityId: id, accountId: "account-\(id)", accountName: "Account \(id)", accountKind: .imap,
        email: "\(id)@EXAMPLE.test", displayName: "Native \(id)", replyTo: "reply-\(id)@example.test",
        signatureSource: "**Signature \(id)**", isDefault: true,
        sendable: sendable, revision: 1)
}
private func nativeContent(route: String? = "a", body: String = "User text") -> DraftContent {
    DraftContent(
        identityId: route, format: .markdown, plainFlowed: false, subject: "Native subject",
        to: [.init(address: "kept@example.test", name: "Kept Name")], cc: [], bcc: [],
        bodySource: body,
        attachments: [
            .retained(
                .init(
                    objectSha256: String(repeating: "a", count: 64), filename: "retained.txt",
                    mediaType: "text/plain", size: 3))
        ])
}

/// A synthetic owned-service facade for native interaction/undo, not disk or
/// real-account qualification. Only the scripted raw input is recognized;
/// actual mailbox grammar/persistence is covered by ComposeWindowTests.
private actor NativeIdentitySource: DraftStore, ComposeCatalogSource, RecipientSource {
    enum Seed {
        case fresh
        case saved(DraftContent)
    }
    private var row = Draft(
        draftId: "native-identity", state: .editing, replyToOccurrenceId: 42,
        forwardOfMessageId: nil, revision: nil, attempt: nil, createdAtMs: 0, updatedAtMs: 0)
    private var catalog: ComposeCatalog
    private var catalogFailure: CoreFailure?
    private var catalogGate: NativeIdentityGate?
    private var parseGate: NativeIdentityGate?
    private var heldParses: [NativeIdentityGate] = []
    private var receipts: [String: DraftSaved] = [:]
    private(set) var catalogReads = 0
    private(set) var parses: [String] = []
    private(set) var saves = 0
    init(_ seed: Seed, defaultId: String?, catalogGate: NativeIdentityGate?) {
        catalog = .init(
            identities: [nativeIdentity("a"), nativeIdentity("b", sendable: .provisional)],
            defaultIdentityId: defaultId)
        self.catalogGate = catalogGate
        if case .saved(let content) = seed { row.revision = Self.revision(content, seq: 0, base: nil) }
    }
    func holdParse(_ gate: NativeIdentityGate?) {
        parseGate = gate
        if let gate { heldParses.append(gate) }
    }
    func failCatalog(_ error: CoreFailure?) { catalogFailure = error }
    func release() async {
        await catalogGate?.open()
        for gate in heldParses { await gate.open() }
    }
    func draft(id: String) async throws -> Draft { row }
    func composeCatalog(preferredAccountId: String?) async throws -> ComposeCatalog {
        catalogReads += 1
        await catalogGate?.wait()  // Deliberately await the actual answer after cancellation.
        if let catalogFailure { throw catalogFailure }
        return catalog
    }
    func parseDraftRecipients(text: String) async throws -> [DraftRecipient] {
        parses.append(text)
        await parseGate?.wait()
        if text.isEmpty { return [] }
        guard text == "next@example.test" else {
            throw CoreFailure.invalidInput(field: "recipients", message: "Invalid scripted mailbox.")
        }
        return [.init(address: text, name: "Next Name")]
    }
    func recipientCompletions(prefix: String, limit: UInt32) async throws -> [DraftRecipient] { [] }
    func saveDraft(
        idempotencyKey: String, draftId: String, baseRevisionId: String?, content: DraftContent
    ) async throws -> DraftSaved {
        if let saved = receipts[idempotencyKey] { return saved }
        guard row.revision?.revisionId == baseRevisionId else {
            throw CoreFailure.conflict(detail: "Synthetic native fixture base changed.")
        }
        for attachment in content.attachments {
            guard case .retained = attachment else {
                throw CoreFailure.invalidInput(field: "attachment", message: "Fixture needs retained input.")
            }
        }
        saves += 1
        row.revision = Self.revision(content, seq: Int64(saves), base: baseRevisionId)
        let saved = DraftSaved(
            receipt: .init(command: "save_draft", resultJson: "{}", createdAtMs: Int64(saves)),
            revisionId: "r\(saves)")
        receipts[idempotencyKey] = saved
        return saved
    }
    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt {
        throw CoreFailure.internalError(detail: "Closing must not discard a saved draft.")
    }
    private static func revision(_ c: DraftContent, seq: Int64, base: String?) -> DraftRevision {
        DraftRevision(
            revisionId: "r\(seq)", seq: seq, baseRevisionId: base,
            accountId: c.identityId.map { "account-\($0)" },
            identityId: c.identityId, format: c.format, plainFlowed: c.plainFlowed, subject: c.subject,
            to: c.to, cc: c.cc, bcc: c.bcc, bodySource: c.bodySource,
            attachments: c.attachments.map {
                guard case .retained(let ref) = $0 else { preconditionFailure("fixture input was checked") }
                return ref
            }, createdAtMs: seq)
    }
}

@MainActor
private func identityEventually(_ check: @MainActor () async -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !(await check()) {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}
@MainActor
private func identityViews(_ view: NSView) -> [NSView] { [view] + view.subviews.flatMap(identityViews) }
@MainActor
private func chooseNative(_ id: String, on controller: ComposeWindowController) throws {
    let popup = controller.identityControls.popup
    let index = try #require(popup.itemArray.firstIndex { ($0.representedObject as? String) == id })
    popup.selectItem(at: index)
    let action = try #require(popup.action)
    #expect(NSApp.sendAction(action, to: popup.target, from: popup))
}
@MainActor
private func signatureButton(_ title: String, on controller: ComposeWindowController) throws -> NSButton {
    let sheet = try #require(controller.window?.attachedSheet)
    let content = try #require(sheet.contentView)
    return try #require(identityViews(content).compactMap { $0 as? NSButton }.first { $0.title == title })
}
private enum IdentityUndoEvents { case application, explicitTestTransactions }

@MainActor
private func withNativeIdentity(
    _ seed: NativeIdentitySource.Seed = .saved(nativeContent()), defaultId: String? = nil,
    catalogGate: NativeIdentityGate? = nil, undoEvents: IdentityUndoEvents = .application,
    appearance: AppearanceModel? = nil, announceRoute: @escaping @MainActor (String) -> Void = { _ in },
    body: @MainActor (ComposeWindowController, NativeIdentitySource) async throws -> Void
) async throws {
    let source = NativeIdentitySource(seed, defaultId: defaultId, catalogGate: catalogGate)
    let controller = ComposeWindowController(
        source: source, draftId: "native-identity", appearance: appearance, announceRoute: announceRoute)
    controller.showWindow(nil)
    let result: Result<Void, any Error>
    do {
        if undoEvents == .explicitTestTransactions {
            // Swift Testing's programmatic actions share one application event.
            // Configure the empty manager before loading; each native From
            // operation must then balance and isolate its own explicit group.
            let undo = try #require(controller.textView.undoManager)
            try #require(undo.groupingLevel == 0)
            undo.groupsByEvent = false
        }
        let loaded = await identityEventually { controller.model.content != nil }
        try #require(loaded)
        try await body(controller, source)
        result = .success(())
    } catch { result = .failure(error) }
    await source.release()
    #expect(await controller.closeWithoutSavingAndWait())
    try result.get()
}

@MainActor
private final class PopupAppearanceFlip {
    let popup: NSPopUpButton
    var fired = false
    init(_ popup: NSPopUpButton) { self.popup = popup }
    func flipDuringSecondIdentity() {
        guard !fired, popup.numberOfItems >= 3 else { return }
        fired = true
        popup.appearance = NSAppearance(named: .darkAqua)
    }
}

private actor NativeStripAppearanceSource: AppearanceSource {
    private var mark = "W"
    private var lightMarker = sampleAccount.tokens.light.markerSrgb
    private var generation: UInt64 = 1
    func setMark(_ value: String) {
        mark = value
        generation += 1
    }
    func setLightMarker(_ value: String) {
        lightMarker = value
        generation += 1
    }
    func appearance() async throws -> Appearance {
        var tokens = sampleAccount.tokens
        tokens.light.markerSrgb = lightMarker
        return Appearance(
            generation: generation, accounts: [],
            identities: ["a", "b"].map { id in
                IdentityAppearance(
                    identityId: id, accountId: "account-\(id)", colorSrgb: "#0072b2",
                    colorOverrideSrgb: nil, tokens: tokens, symbolOverride: nil,
                    monogram: mark, revision: nil)
            })
    }
    func appearanceEvents(generation: UInt64) async throws -> any AppearanceEventSource {
        throw CoreFailure.unavailable(reason: "Not used by this refresh fixture.")
    }
    func setAccountAppearance(
        accountId: String, expectedRevision: Int64, change: AccountAppearanceChange
    ) async throws -> Receipt { throw CoreFailure.unavailable(reason: "Not used by this fixture.") }
    func setIdentityAppearance(
        identityId: String, expectedRevision: Int64?, change: IdentityAppearanceChange
    ) async throws -> Receipt { throw CoreFailure.unavailable(reason: "Not used by this fixture.") }
    nonisolated func palette() -> [PaletteColor] { [] }
}

@Suite(.serialized, .timeLimit(.minutes(1))) @MainActor
struct ComposeIdentityNativeTests {
    @Test func sharedAppearanceUpdatesTwoWindowsWithoutEditingTheirDrafts() async throws {
        let source = NativeStripAppearanceSource()
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        var announcements: [String] = []
        let announce: @MainActor (String) -> Void = { announcements.append($0) }
        try await withNativeIdentity(appearance: appearance, announceRoute: announce) {
            first, _ in
            try await withNativeIdentity(appearance: appearance, announceRoute: announce) {
                second, _ in
                let expected = "W · Account a · Native a <a@EXAMPLE.test>"
                try #require(
                    await identityEventually {
                        first.identityStrip.strip.accessibilityLabel() == expected
                            && second.identityStrip.strip.accessibilityLabel() == expected
                    })
                #expect(
                    first.window?.titlebarAccessoryViewControllers.contains { $0 === first.identityStrip }
                        == true)
                let firstContent = first.model.content
                let secondContent = second.model.content
                let firstDirty = first.model.isDirty
                let firstUndo = first.textView.undoManager?.canUndo
                let secondUndo = second.textView.undoManager?.canUndo
                await source.setMark("X")
                await appearance.refresh()
                try #require(
                    await identityEventually {
                        first.identityStrip.strip.accessibilityLabel()?.hasPrefix("X ·") == true
                            && second.identityStrip.strip.accessibilityLabel()?.hasPrefix("X ·") == true
                    })
                #expect(first.model.content == firstContent)
                #expect(second.model.content == secondContent)
                #expect(first.model.isDirty == firstDirty)
                #expect(first.textView.undoManager?.canUndo == firstUndo)
                #expect(second.textView.undoManager?.canUndo == secondUndo)
                #expect(announcements.isEmpty)
            }
        }
    }

    @Test func popupSwatchesFollowTokensAndAppearanceWithoutEditingTheDraft() async throws {
        let source = NativeStripAppearanceSource()
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        var announcements: [String] = []
        let announce: @MainActor (String) -> Void = { announcements.append($0) }
        try await withNativeIdentity(appearance: appearance, announceRoute: announce) { controller, _ in
            let popup = controller.identityControls.popup
            popup.appearance = NSAppearance(named: .aqua)
            try #require(
                await identityEventually {
                    popup.itemArray.contains { ($0.representedObject as? String) == "a" && $0.image != nil }
                })
            let a = try #require(popup.itemArray.first { ($0.representedObject as? String) == "a" })
            let b = try #require(popup.itemArray.first { ($0.representedObject as? String) == "b" })
            #expect(popup.item(at: 0)?.image == nil)
            #expect((popup.selectedItem?.representedObject as? String) == "a")
            #expect(b.title.contains("b@EXAMPLE.test") && b.title.contains("provisional"))
            let image = try #require(a.image)
            #expect(!image.isTemplate && b.image?.isTemplate == false)
            #expect(try swatchPixel(image, x: 0.5, y: 0.5).redComponent < 0.1)
            controller.identityControls.render()
            #expect(popup.selectedItem?.image === image)
            let content = controller.model.content
            let dirty = controller.model.isDirty
            let undo = controller.textView.undoManager?.canUndo
            await source.setLightMarker("#d55e00")
            await appearance.refresh()
            try #require(
                await identityEventually {
                    guard let image = popup.selectedItem?.image,
                        let color = try? swatchPixel(image, x: 0.5, y: 0.5)
                    else { return false }
                    return color.redComponent > 0.7
                })
            popup.appearance = NSAppearance(named: .darkAqua)
            try #require(
                await identityEventually {
                    guard let image = popup.selectedItem?.image,
                        let color = try? swatchPixel(image, x: 0.5, y: 0.5)
                    else { return false }
                    return abs(color.redComponent - 0x56 / 255.0) < 0.02
                })
            #expect(controller.model.content == content)
            #expect(controller.model.isDirty == dirty)
            #expect(controller.textView.undoManager?.canUndo == undo)
            #expect(announcements.isEmpty)
            var mismatched = try #require(appearance.appearance)
            mismatched.identities[0].accountId = "wrong-account"
            controller.identityControls.appearanceSnapshot = mismatched
            controller.identityControls.render()
            #expect(popup.selectedItem?.image == nil)
            #expect(popup.itemArray.first { ($0.representedObject as? String) == "b" }?.image != nil)
            controller.identityControls.appearanceSnapshot = nil
            controller.identityControls.render()
            #expect(popup.itemArray.allSatisfy { $0.image == nil })
            #expect((popup.selectedItem?.representedObject as? String) == "a")
        }
    }

    @Test func appearanceChangesDuringMenuRebuildAreNotLost() async throws {
        let source = NativeStripAppearanceSource()
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        try await withNativeIdentity(appearance: appearance) { controller, _ in
            let popup = controller.identityControls.popup
            popup.appearance = NSAppearance(named: .aqua)
            try #require(
                await identityEventually {
                    popup.itemArray.contains { ($0.representedObject as? String) == "b" && $0.image != nil }
                })
            let menu = try #require(popup.menu)
            let probe = PopupAppearanceFlip(popup)
            let observer = NotificationCenter.default.addObserver(
                forName: NSMenu.didAddItemNotification, object: menu, queue: nil
            ) { _ in
                MainActor.assumeIsolated { probe.flipDuringSecondIdentity() }
            }
            defer { NotificationCenter.default.removeObserver(observer) }
            controller.identityControls.render()
            try #require(probe.fired)
            try #require(
                await identityEventually {
                    guard let image = popup.selectedItem?.image,
                        let color = try? swatchPixel(image, x: 0.5, y: 0.5)
                    else { return false }
                    return abs(color.redComponent - 0x56 / 255.0) < 0.02
                })
        }
    }

    @Test func appearanceRefreshDoesNotCommitMarkedTextOrChangeFocus() async throws {
        let source = NativeStripAppearanceSource()
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        try await withNativeIdentity(.saved(nativeContent(body: "")), appearance: appearance) {
            controller, _ in
            try #require(await identityEventually { controller.identityControls.model.isReady })
            controller.window?.makeFirstResponder(controller.textView)
            controller.textView.setMarkedText(
                "編集中", selectedRange: .init(location: 3, length: 0),
                replacementRange: .init(location: 0, length: 0))
            try #require(controller.textView.hasMarkedText())
            let markedRange = controller.textView.markedRange()
            let nativeBytes = Array(controller.textView.string.utf8)
            let sourceBefore = controller.model.content
            let undoBefore = controller.textView.undoManager?.canUndo
            await source.setMark("X")
            await appearance.refresh()
            try #require(
                await identityEventually {
                    controller.identityStrip.strip.accessibilityLabel()?.hasPrefix("X ·") == true
                })
            #expect(controller.textView.hasMarkedText())
            #expect(controller.textView.markedRange() == markedRange)
            #expect(Array(controller.textView.string.utf8) == nativeBytes)
            #expect(controller.window?.firstResponder === controller.textView)
            #expect(controller.model.content == sourceBefore)
            #expect(controller.textView.undoManager?.canUndo == undoBefore)
            controller.textView.unmarkText()
        }
    }

    @Test func committedRouteAndUndoEachAnnounceOnce() async throws {
        let source = NativeStripAppearanceSource()
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        var announcements: [String] = []
        let announce: @MainActor (String) -> Void = { announcements.append($0) }
        try await withNativeIdentity(
            .saved(nativeContent(body: "")), undoEvents: .explicitTestTransactions,
            appearance: appearance, announceRoute: announce
        ) { controller, _ in
            try #require(
                await identityEventually {
                    controller.identityControls.popup.itemArray.contains {
                        ($0.representedObject as? String) == "b"
                    }
                })
            try chooseNative("b", on: controller)
            try #require(await identityEventually { controller.model.content?.identityId == "b" })
            #expect(controller.identityStrip.strip.accessibilityLabel()?.contains("b@EXAMPLE.test") == true)
            #expect(announcements.count == 1)
            #expect(announcements.first?.contains("b@EXAMPLE.test") == true)
            let undo = try #require(controller.textView.undoManager)
            undo.undo()
            #expect(controller.model.content?.identityId == "a")
            #expect(controller.identityStrip.strip.accessibilityLabel()?.contains("a@EXAMPLE.test") == true)
            #expect(announcements.count == 2)
            undo.redo()
            #expect(controller.model.content?.identityId == "b")
            #expect(announcements.count == 3)
        }
    }

    @Test func unchangedNativeTextSharesOneEventUnlessTestTransactionsAreExplicit() throws {
        let window = NSWindow(
            contentRect: .init(x: 0, y: 0, width: 320, height: 160),
            styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        let editor = NSTextView(frame: window.contentView?.bounds ?? .zero)
        editor.allowsUndo = true
        window.contentView = editor
        let undo = try #require(editor.undoManager)
        #expect(undo.groupsByEvent)
        @MainActor func replace(_ text: String) {
            editor.breakUndoCoalescing()
            undo.beginUndoGrouping()
            editor.insertText(text, replacementRange: .init(location: 0, length: editor.string.utf16.count))
            undo.endUndoGrouping()
            editor.breakUndoCoalescing()
        }
        replace("a")
        replace("b")
        undo.undo()
        #expect(editor.string.isEmpty)
        try #require(undo.groupingLevel == 0)
        undo.groupsByEvent = false
        replace("a")
        replace("b")
        undo.undo()
        #expect(editor.string == "a")
        #expect(undo.canRedo)
        undo.redo()
        #expect(editor.string == "b")
    }

    @Test func freshDefaultAndNativeSignatureRouteUndoRedoStayTogether() async throws {
        try await withNativeIdentity(.fresh, defaultId: "a", undoEvents: .explicitTestTransactions) {
            c, source in
            let ready = await identityEventually { c.model.content?.identityId == "a" }
            try #require(ready)
            #expect(c.textView.string == "**Signature a**")
            #expect(c.model.content?.format == .plain)
            #expect(c.textView.mode == .plain)
            #expect(c.identityControls.popup.selectedItem?.title.contains("a@EXAMPLE.test") == true)
            #expect(await c.saveAndWait())
            try chooseNative("b", on: c)
            let changed = await identityEventually { c.model.content?.identityId == "b" }
            try #require(changed)
            #expect(c.textView.string == "**Signature b**")
            #expect(c.window?.title.contains("b@EXAMPLE.test") == true)
            #expect(c.identityControls.popup.selectedItem?.title.contains("provisional") == true)
            #expect(await c.saveAndWait())
            let before = try await source.draft(id: c.model.draftId).revision?.revisionId
            let undo = try #require(c.textView.undoManager)
            #expect(undo.canUndo)
            undo.undo()
            #expect(c.model.content?.identityId == "a")
            #expect(c.textView.string == "**Signature a**")
            #expect(await c.saveAndWait())
            #expect(try await source.draft(id: c.model.draftId).revision?.revisionId != before)
            #expect(undo.canRedo)
            undo.redo()
            #expect(c.model.content?.identityId == "b")
            #expect(c.textView.string == "**Signature b**")
            #expect(c.textView.isOnTextKit2)
        }
    }

    @Test func keepTextIsTheSafeDefaultAndAppendRemainsExplicitAndUndoable() async throws {
        try await withNativeIdentity(undoEvents: .explicitTestTransactions) { c, source in
            let ready = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(ready)
            try chooseNative("b", on: c)
            let deciding = await identityEventually { c.window?.attachedSheet != nil }
            try #require(deciding)
            let keep = try signatureButton("Keep Text", on: c)
            #expect(c.window?.attachedSheet?.defaultButtonCell === keep.cell)
            #expect(c.model.content?.identityId == "a")
            keep.performClick(nil)
            let kept = await identityEventually {
                c.model.content?.identityId == "b" && c.window?.attachedSheet == nil
            }
            try #require(kept)
            #expect(c.textView.string == "User text")
            try chooseNative("a", on: c)
            let again = await identityEventually { c.window?.attachedSheet != nil }
            try #require(again)
            try signatureButton("Append Signature", on: c).performClick(nil)
            let appended = await identityEventually { c.model.content?.identityId == "a" }
            try #require(appended)
            #expect(c.textView.string == "User text\n\n**Signature a**")
            #expect(await c.saveAndWait())
            let saved = try await source.draft(id: c.model.draftId).revision
            #expect(saved?.to.first?.name == "Kept Name")
            #expect(saved?.attachments.first?.filename == "retained.txt")
            #expect(saved?.plainFlowed == false)
            let undo = try #require(c.textView.undoManager)
            undo.undo()
            #expect(c.model.content?.identityId == "b")
            #expect(c.textView.string == "User text")
            #expect(c.model.content?.subject == "Native subject")
            #expect(undo.canRedo)
            undo.redo()
            #expect(c.model.content?.identityId == "a")
            #expect(c.textView.string == "User text\n\n**Signature a**")
        }
    }

    @Test func aSignatureSheetPausesAutosaveAndCloseCancelsOnlyItsProposal() async throws {
        try await withNativeIdentity { c, source in
            let ready = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(ready)
            c.subjectField.stringValue = "Later subject"
            c.controlTextDidChange(.init(name: NSControl.textDidChangeNotification, object: c.subjectField))
            try chooseNative("b", on: c)
            let deciding = await identityEventually { c.identityControls.isDeciding }
            try #require(deciding)
            c.windowDidResignKey(.init(name: NSWindow.didResignKeyNotification, object: c.window))
            try await Task.sleep(for: .milliseconds(1700))
            #expect(await source.saves == 0)
            #expect(!(await c.saveAndWait()))
            #expect(await c.closeAndWait())
            let saved = try await source.draft(id: c.model.draftId).revision
            #expect(saved?.identityId == "a")
            #expect(saved?.bodySource == "User text")
            #expect(saved?.subject == "Later subject")
            #expect(saved?.attachments.first?.filename == "retained.txt")
        }
    }

    @Test func aSignatureChoiceAlsoPausesAnAlreadyPreparingAutosave() async throws {
        try await withNativeIdentity { c, source in
            let ready = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(ready)
            let gate = NativeIdentityGate()
            await source.holdParse(gate)
            let field = c.toRecipients.control
            try #require(c.window?.makeFirstResponder(field) == true)
            let editor = try #require(field.currentEditor() as? NSTextView)
            editor.insertText(
                "next@example.test", replacementRange: .init(location: 0, length: editor.string.utf16.count))
            let save = Task { await c.saveAndWait() }
            let began = await identityEventually { await source.parses.contains("next@example.test") }
            try #require(began)
            await source.holdParse(nil)
            try chooseNative("b", on: c)
            let deciding = await identityEventually { c.window?.attachedSheet != nil }
            try #require(deciding)
            await gate.open()
            #expect(!(await save.value))
            #expect(await source.saves == 0)
            #expect(c.model.content?.identityId == "a")
            #expect(c.model.isDirty)
            #expect(c.identityControls.isDeciding)
        }
    }

    @Test func frozenOrMarkedNativeTextRefusesTheProposedRouteAndBody() async throws {
        try await withNativeIdentity(.saved(nativeContent(body: ""))) { c, _ in
            let ready = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(ready)
            c.textView.isEditable = false
            #expect(c.identityControls.model.choose(identityId: "b") == .refused)
            #expect(c.model.content?.identityId == "a")
            #expect(c.textView.string.isEmpty)
            c.textView.isEditable = true
            c.textView.setMarkedText(
                "編集中", selectedRange: .init(location: 3, length: 0),
                replacementRange: .init(location: 0, length: 0))
            #expect(c.textView.hasMarkedText())
            #expect(c.identityControls.model.choose(identityId: "b") == .refused)
            #expect(c.model.content?.identityId == "a")
            #expect(c.textView.hasMarkedText())
            c.textView.unmarkText()
        }
    }

    @Test func abandonmentAwaitsCatalogAndNeverAppliesItsLateDefault() async throws {
        let gate = NativeIdentityGate()
        try await withNativeIdentity(.fresh, defaultId: "a", catalogGate: gate) { c, source in
            let began = await identityEventually { await source.catalogReads == 1 }
            try #require(began)
            var finished = false
            let close = Task {
                let result = await c.closeWithoutSavingAndWait()
                finished = true
                return result
            }
            close.cancel()
            try await Task.sleep(for: .milliseconds(20))
            #expect(!finished)
            await gate.open()
            #expect(await close.value)
            #expect(c.model.content?.identityId == nil)
            #expect(await source.saves == 0)
        }
    }

    @Test func abandonmentDrainsBegunSelectionWithoutANativeWrite() async throws {
        try await withNativeIdentity(.saved(nativeContent(body: ""))) { c, source in
            let ready = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(ready)
            let gate = NativeIdentityGate()
            await source.holdParse(gate)
            let field = c.toRecipients.control
            try #require(c.window?.makeFirstResponder(field) == true)
            let editor = try #require(field.currentEditor() as? NSTextView)
            editor.insertText(
                "next@example.test", replacementRange: .init(location: 0, length: editor.string.utf16.count))
            c.identityControls.choose(identityId: "b")
            let began = await identityEventually { await source.parses.contains("next@example.test") }
            try #require(began)
            var finished = false
            let close = Task {
                let result = await c.closeWithoutSavingAndWait()
                finished = true
                return result
            }
            try await Task.sleep(for: .milliseconds(20))
            #expect(!finished)
            await gate.open()
            #expect(await close.value)
            #expect(c.model.content?.identityId == "a")
            #expect(await source.saves == 0)
        }
    }

    @Test func invalidRawInputRefusesFromUntilTheWholeCurrentInputIsReady() async throws {
        try await withNativeIdentity(.saved(nativeContent(body: ""))) { c, _ in
            let ready = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(ready)
            let field = c.toRecipients.control
            try #require(c.window?.makeFirstResponder(field) == true)
            let editor = try #require(field.currentEditor() as? NSTextView)
            editor.insertText(
                "broken", replacementRange: .init(location: 0, length: editor.string.utf16.count))
            try chooseNative("b", on: c)
            let refused = await identityEventually { c.identityControls.problem != nil }
            try #require(refused)
            #expect(c.model.content?.identityId == "a")
            #expect(editor.string.contains("broken"))
            #expect(c.toRecipients.isPending)
            #expect(!(await c.saveAndWait()))
            editor.insertText(
                "next@example.test", replacementRange: .init(location: 0, length: editor.string.utf16.count))
            try chooseNative("b", on: c)
            let chosen = await identityEventually { c.model.content?.identityId == "b" }
            try #require(chosen)
            #expect(c.model.content?.to == [.init(address: "next@example.test", name: "Next Name")])
            #expect(c.model.content?.subject == "Native subject")
            #expect(c.model.content?.attachments.count == 1)
        }
    }

    @Test func supersededSelectionPreservesLaterBodyHeadersAndRecipients() async throws {
        try await withNativeIdentity(.saved(nativeContent(body: ""))) { c, source in
            let ready = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(ready)
            let gate = NativeIdentityGate()
            await source.holdParse(gate)
            let field = c.toRecipients.control
            try #require(c.window?.makeFirstResponder(field) == true)
            let editor = try #require(field.currentEditor() as? NSTextView)
            editor.insertText(
                "next@example.test", replacementRange: .init(location: 0, length: editor.string.utf16.count))
            try chooseNative("b", on: c)
            let began = await identityEventually { await source.parses.contains("next@example.test") }
            try #require(began)
            c.subjectField.stringValue = "Latest header"
            c.controlTextDidChange(.init(name: NSControl.textDidChangeNotification, object: c.subjectField))
            c.textView.insertText("Latest body", replacementRange: .init(location: 0, length: 0))
            c.textDidChange(.init(name: NSText.didChangeNotification, object: c.textView))
            try chooseNative("a", on: c)
            await gate.open()
            let settled = await identityEventually {
                c.model.content?.to.first?.address == "next@example.test"
            }
            try #require(settled)
            #expect(c.model.content?.identityId == "a")
            #expect(c.model.content?.subject == "Latest header")
            #expect(c.model.content?.bodySource == "Latest body")
            #expect(c.window?.attachedSheet == nil)
            #expect(c.identityControls.problem == nil)
            #expect(c.model.content?.attachments.count == 1)
        }
    }

    @Test func catalogFailureIsVisibleAndTheNativeRefreshButtonRecoversIt() async throws {
        try await withNativeIdentity { c, source in
            let ready = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(ready)
            await source.failCatalog(.unavailable(reason: "Scripted catalog failure"))
            let button = c.identityControls.refreshButton
            let action = try #require(button.action)
            #expect(NSApp.sendAction(action, to: button.target, from: button))
            let failed = await identityEventually {
                if case .failed = c.identityControls.model.readState { return true }
                return false
            }
            try #require(failed)
            #expect(c.statusLabel.stringValue.contains("Scripted catalog failure"))
            #expect(!c.identityControls.popup.isEnabled)
            #expect(c.model.content?.identityId == "a")
            await source.failCatalog(nil)
            #expect(NSApp.sendAction(action, to: button.target, from: button))
            let retried = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(retried)
            #expect(await source.catalogReads == 3)
            #expect(c.textView.string == "User text")
        }
    }

    @Test func aCleanReloadCannotApplyConsentForThePreviouslyDisplayedBody() async throws {
        try await withNativeIdentity { c, source in
            let ready = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(ready)
            try chooseNative("b", on: c)
            let deciding = await identityEventually { c.window?.attachedSheet != nil }
            try #require(deciding)
            var replacement = nativeContent(body: "Newly loaded body")
            replacement.format = .plain
            _ = try await source.saveDraft(
                idempotencyKey: "another-writer", draftId: c.model.draftId,
                baseRevisionId: c.model.baseRevisionId, content: replacement)
            await c.model.reload()
            let displayed = await identityEventually { c.textView.string == replacement.bodySource }
            try #require(displayed)
            try signatureButton("Append Signature", on: c).performClick(nil)
            let dismissed = await identityEventually { c.window?.attachedSheet == nil }
            try #require(dismissed)
            #expect(c.model.content?.identityId == "a")
            #expect(c.model.content?.bodySource == replacement.bodySource)
            #expect(c.textView.string == replacement.bodySource)
            #expect(c.model.content?.format == .plain)
            #expect(!c.model.isDirty)
            #expect(c.statusLabel.stringValue.contains("changed"))
        }
    }

    @Test func anUnavailableSavedRouteIsNotReplacedByTheCatalogDefault() async throws {
        try await withNativeIdentity(.saved(nativeContent(route: "missing")), defaultId: "a") { c, _ in
            let ready = await identityEventually { c.identityControls.popup.isEnabled }
            try #require(ready)
            #expect(c.model.content?.identityId == "missing")
            #expect(c.identityControls.popup.selectedItem?.title == "Unavailable identity: missing")
            #expect(c.textView.string == "User text")
        }
    }
}
