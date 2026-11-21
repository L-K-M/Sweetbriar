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
    private let draftFailure: CoreFailure?
    private var catalogGate: NativeIdentityGate?
    private var parseGate: NativeIdentityGate?
    private var heldParses: [NativeIdentityGate] = []
    private var receipts: [String: DraftSaved] = [:]
    private(set) var catalogReads = 0
    private(set) var parses: [String] = []
    private(set) var saves = 0
    init(_ seed: Seed, defaultId: String?, catalogGate: NativeIdentityGate?, draftFailure: CoreFailure?) {
        self.draftFailure = draftFailure
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
    func draft(id: String) async throws -> Draft {
        if let draftFailure { throw draftFailure }
        return row
    }
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
    func publishDraftAttachment(file: URL) async throws -> DraftAttachmentObject {
        throw CoreFailure.internalError(detail: "attachment publication is not used by this fixture")
    }
    func republishDraftAttachment(
        draftId: String, revisionId: String, reference: DraftAttachmentRef
    ) async throws -> DraftAttachmentObject {
        throw CoreFailure.internalError(detail: "attachment republication is not used by this fixture")
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
private func identityEventually(
    within limit: Duration = .seconds(10), _ check: @MainActor () async -> Bool
) async -> Bool {
    let deadline = ContinuousClock.now + limit
    while !(await check()) {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}
@MainActor
private func identityViews(_ view: NSView) -> [NSView] { [view] + view.subviews.flatMap(identityViews) }
@MainActor
private func editorBackgroundPixel(_ view: NSTextView) throws -> NSColor {
    let bitmap = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
    try #require(bitmap.pixelsWide >= 4 && bitmap.pixelsHigh > 0, "Editor must have a laid-out surface")
    view.cacheDisplay(in: view.bounds, to: bitmap)
    let pixel = try #require(bitmap.colorAt(x: bitmap.pixelsWide - 4, y: bitmap.pixelsHigh / 2))
    return try #require(pixel.usingColorSpace(.sRGB))
}
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
/// A full-screen transition animates a new Space in; on CI runners it
/// has taken longer than the 10 s other checks allow.
private let fullScreenTransition: Duration = .seconds(30)

@MainActor
private final class NativeFullScreenEvents: NSObject {
    private(set) var entered: Set<ObjectIdentifier> = []
    private(set) var exited: Set<ObjectIdentifier> = []

    override init() {
        super.init()
        let center = NotificationCenter.default
        center.addObserver(
            self, selector: #selector(didEnter), name: NSWindow.didEnterFullScreenNotification, object: nil)
        center.addObserver(
            self, selector: #selector(didExit), name: NSWindow.didExitFullScreenNotification, object: nil)
    }

    @objc private func didEnter(_ notification: Notification) {
        if let window = notification.object as? NSWindow { entered.insert(ObjectIdentifier(window)) }
    }

    @objc private func didExit(_ notification: Notification) {
        if let window = notification.object as? NSWindow { exited.insert(ObjectIdentifier(window)) }
    }
}

@MainActor
private func withFullScreenWindows(
    _ windows: [NSWindow], body: @MainActor () async throws -> Void
) async throws {
    let events = NativeFullScreenEvents()
    let result: Result<Void, any Error>
    do {
        for window in windows {
            window.makeKeyAndOrderFront(nil)
            window.toggleFullScreen(nil)
            try #require(
                await identityEventually(within: fullScreenTransition) {
                    events.entered.contains(ObjectIdentifier(window))
                        && window.styleMask.contains(.fullScreen)
                })
        }
        try await body()
        result = .success(())
    } catch { result = .failure(error) }
    for window in windows.reversed() where window.styleMask.contains(.fullScreen) {
        window.makeKeyAndOrderFront(nil)
        window.toggleFullScreen(nil)
        #expect(
            await identityEventually(within: fullScreenTransition) {
                events.exited.contains(ObjectIdentifier(window)) && !window.styleMask.contains(.fullScreen)
            })
    }
    try result.get()
}

private enum IdentityUndoEvents { case application, explicitTestTransactions }

@MainActor
private func withNativeIdentity(
    _ seed: NativeIdentitySource.Seed = .saved(nativeContent()), defaultId: String? = nil,
    catalogGate: NativeIdentityGate? = nil, undoEvents: IdentityUndoEvents = .application,
    draftFailure: CoreFailure? = nil, appearance: AppearanceModel? = nil, cue: ComposeCueModel? = nil,
    announceRoute: @escaping @MainActor (String) -> Void = { _ in },
    body: @MainActor (ComposeWindowController, NativeIdentitySource) async throws -> Void
) async throws {
    let source = NativeIdentitySource(
        seed, defaultId: defaultId, catalogGate: catalogGate, draftFailure: draftFailure)
    let controller = ComposeWindowController(
        source: source, draftId: "native-identity", appearance: appearance, cue: cue,
        announceRoute: announceRoute)
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
        let settled = await identityEventually {
            draftFailure == nil ? controller.model.content != nil : controller.model.failure != nil
        }
        try #require(settled)
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
    private var lightStrip = sampleAccount.tokens.light.stripSrgb
    private var darkStrip = sampleAccount.tokens.dark.stripSrgb
    private var failure: CoreFailure?
    private var generation: UInt64 = 1
    func setMark(_ value: String) {
        mark = value
        generation += 1
    }
    func setLightMarker(_ value: String) {
        lightMarker = value
        generation += 1
    }
    func setFailure(_ error: CoreFailure?) { failure = error }
    func setStrips(light: String, dark: String) {
        lightStrip = light
        darkStrip = dark
        generation += 1
    }
    func appearance() async throws -> Appearance {
        if let failure { throw failure }
        var tokens = sampleAccount.tokens
        tokens.light.markerSrgb = lightMarker
        tokens.light.stripSrgb = lightStrip
        tokens.dark.stripSrgb = darkStrip
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

private actor NativeCueSource: ComposeCueSource {
    private var value = ComposeCueMode.standard
    private var failure: CoreFailure?
    func setFailure(_ error: CoreFailure?) { failure = error }
    func composeCue() async throws -> ComposeCueMode {
        if let failure { throw failure }
        return value
    }
    func setComposeCue(_ mode: ComposeCueMode) async throws -> Receipt {
        if let failure { throw failure }
        value = mode
        return .init(command: "set_setting", resultJson: "{}", createdAtMs: 1)
    }
}

@Suite(.serialized, .timeLimit(.minutes(1))) @MainActor
struct ComposeIdentityNativeTests {
    @Test func twoFullScreenWindowsKeepTheirIdentityAndSharedAppearance() async throws {
        let colors = NativeStripAppearanceSource()
        let appearance = AppearanceModel(source: colors)
        await appearance.refresh()
        let cue = ComposeCueModel(source: NativeCueSource())
        await cue.reload()
        var announcements: [String] = []
        let announce: @MainActor (String) -> Void = { announcements.append($0) }
        try await withNativeIdentity(
            .saved(nativeContent(body: "")), appearance: appearance, cue: cue, announceRoute: announce
        ) { first, _ in
            try await withNativeIdentity(
                .saved(nativeContent(body: "")), appearance: appearance, cue: cue, announceRoute: announce
            ) { second, _ in
                let controllers = [first, second]
                try #require(
                    await identityEventually {
                        controllers.allSatisfy {
                            $0.identityStrip.strip.accessibilityLabel()?.contains("a@EXAMPLE.test") == true
                        }
                    })
                let windows = try controllers.map { try #require($0.window) }
                for (index, controller) in controllers.enumerated() {
                    windows[index].makeKeyAndOrderFront(nil)
                    windows[index].makeFirstResponder(controller.textView)
                    controller.textView.insertText(
                        "Typed \(index)", replacementRange: .init(location: 0, length: 0))
                    #expect(await controller.saveAndWait())
                    try #require(controller.textView.undoManager?.canUndo == true)
                }
                let contents = controllers.map { $0.model.content }
                let sourceText = controllers.map { Array($0.textView.string.utf8) }
                let dirty = controllers.map { $0.model.isDirty }
                let undo = controllers.map { $0.textView.undoManager?.canUndo }
                let redo = controllers.map { $0.textView.undoManager?.canRedo }
                try await withFullScreenWindows(windows) {
                    #expect(windows.allSatisfy { $0.styleMask.contains(.fullScreen) })
                    for controller in controllers {
                        let strip = controller.identityStrip
                        let initialHeight = strip.view.frame.height
                        #expect(initialHeight >= 32 && strip.fullScreenMinHeight == initialHeight)
                        strip.setTextFont(.systemFont(ofSize: 26))
                        #expect(strip.view.frame.height > initialHeight)
                        #expect(strip.fullScreenMinHeight == strip.view.frame.height)
                        #expect(strip.strip.accessibilityLabel()?.contains("a@EXAMPLE.test") == true)
                    }
                    #expect(await cue.setMode(.strong))
                    try #require(
                        await identityEventually {
                            windows.allSatisfy {
                                $0.styleMask.contains(.fullScreen)
                                    && $0.styleMask.contains(.fullSizeContentView)
                                    && $0.titlebarAppearsTransparent
                            }
                        })
                    await colors.setMark("X")
                    await appearance.refresh()
                    try #require(
                        await identityEventually {
                            controllers.allSatisfy {
                                $0.identityStrip.strip.accessibilityLabel()?.hasPrefix("X ·") == true
                            }
                        })
                    #expect(controllers.allSatisfy { $0.textView.drawsBackground })
                    #expect(controllers.allSatisfy { $0.textView.backgroundColor == .textBackgroundColor })
                    #expect(controllers.map { $0.model.content } == contents)
                    #expect(controllers.map { Array($0.textView.string.utf8) } == sourceText)
                    #expect(controllers.map { $0.model.isDirty } == dirty)
                    #expect(controllers.map { $0.textView.undoManager?.canUndo } == undo)
                    #expect(controllers.map { $0.textView.undoManager?.canRedo } == redo)
                    #expect(announcements.isEmpty)
                }
                #expect(windows.allSatisfy { !$0.styleMask.contains(.fullScreen) })
                #expect(controllers.map { $0.model.content } == contents)
                #expect(controllers.map { $0.model.isDirty } == dirty)
                #expect(controllers.map { $0.textView.undoManager?.canUndo } == undo)
                #expect(controllers.map { $0.textView.undoManager?.canRedo } == redo)
                for (index, controller) in controllers.enumerated() {
                    let manager = try #require(controller.textView.undoManager)
                    manager.undo()
                    #expect(controller.textView.string.isEmpty)
                    try #require(manager.canRedo)
                    manager.redo()
                    #expect(controller.textView.string == "Typed \(index)")
                }
            }
        }
    }

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

    @Test func sharedCueTintsTwoWindowsWithoutTouchingMarkedTextOrUndo() async throws {
        let colors = NativeStripAppearanceSource()
        await colors.setStrips(light: "#eaf3ff", dark: "#1c2530")
        let appearance = AppearanceModel(source: colors)
        await appearance.refresh()
        let cueSource = NativeCueSource()
        let cue = ComposeCueModel(source: cueSource)
        await cue.reload()
        var announcements: [String] = []
        let announce: @MainActor (String) -> Void = { announcements.append($0) }
        try await withNativeIdentity(
            .saved(nativeContent(body: "")), appearance: appearance, cue: cue, announceRoute: announce
        ) { first, _ in
            try await withNativeIdentity(
                .saved(nativeContent(body: "")), appearance: appearance, cue: cue, announceRoute: announce
            ) { second, _ in
                first.window?.appearance = NSAppearance(named: .aqua)
                second.window?.appearance = NSAppearance(named: .darkAqua)
                try #require(
                    await identityEventually {
                        first.identityStrip.strip.accessibilityLabel()?.contains("a@EXAMPLE.test") == true
                            && second.identityStrip.strip.accessibilityLabel()?.contains("a@EXAMPLE.test")
                                == true
                    })
                #expect(first.window?.styleMask.contains(.fullSizeContentView) == false)
                #expect(second.window?.styleMask.contains(.fullSizeContentView) == false)
                first.window?.makeKeyAndOrderFront(nil)
                first.window?.makeFirstResponder(first.textView)
                first.textView.setMarkedText(
                    "編集中", selectedRange: .init(location: 3, length: 0),
                    replacementRange: .init(location: 0, length: 0))
                try #require(first.textView.hasMarkedText())
                let marked = first.textView.markedRange()
                let nativeBytes = Array(first.textView.string.utf8)
                let firstContent = first.model.content
                let secondContent = second.model.content
                let firstManager = try #require(first.textView.undoManager)
                let secondManager = try #require(second.textView.undoManager)
                let firstUndo = firstManager.canUndo
                let secondUndo = secondManager.canUndo
                let firstRedo = firstManager.canRedo
                let secondRedo = secondManager.canRedo
                let editorBefore = try editorBackgroundPixel(first.textView)
                #expect(await cue.setMode(.strong))
                try #require(
                    await identityEventually {
                        guard let light = first.window?.backgroundColor.usingColorSpace(.sRGB),
                            let dark = second.window?.backgroundColor.usingColorSpace(.sRGB)
                        else { return false }
                        return first.window?.styleMask.contains(.fullSizeContentView) == true
                            && second.window?.styleMask.contains(.fullSizeContentView) == true
                            && abs(light.redComponent - 0xea / 255.0) < 0.01
                            && abs(dark.redComponent - 0x1c / 255.0) < 0.01
                    })
                let light = try #require(first.window?.backgroundColor.usingColorSpace(.sRGB))
                let dark = try #require(second.window?.backgroundColor.usingColorSpace(.sRGB))
                #expect(abs(light.redComponent - 0xea / 255.0) < 0.01)
                #expect(abs(light.greenComponent - 0xf3 / 255.0) < 0.01)
                #expect(abs(light.blueComponent - 0xff / 255.0) < 0.01)
                #expect(abs(dark.redComponent - 0x1c / 255.0) < 0.01)
                #expect(abs(dark.greenComponent - 0x25 / 255.0) < 0.01)
                #expect(abs(dark.blueComponent - 0x30 / 255.0) < 0.01)
                #expect(first.window?.titlebarAppearsTransparent == true)
                #expect(second.window?.titlebarAppearsTransparent == true)
                second.window?.appearance = NSAppearance(named: .aqua)
                try #require(
                    await identityEventually {
                        guard let value = second.window?.backgroundColor.usingColorSpace(.sRGB) else {
                            return false
                        }
                        return abs(value.redComponent - 0xea / 255.0) < 0.01
                    })
                second.window?.appearance = NSAppearance(named: .darkAqua)
                try #require(
                    await identityEventually {
                        guard let value = second.window?.backgroundColor.usingColorSpace(.sRGB) else {
                            return false
                        }
                        return abs(value.redComponent - 0x1c / 255.0) < 0.01
                    })
                #expect(first.textView.drawsBackground && second.textView.drawsBackground)
                #expect(first.textView.backgroundColor == .textBackgroundColor)
                #expect(second.textView.backgroundColor == .textBackgroundColor)
                let editorAfter = try editorBackgroundPixel(first.textView)
                #expect(abs(editorAfter.redComponent - editorBefore.redComponent) < 0.02)
                #expect(abs(editorAfter.greenComponent - editorBefore.greenComponent) < 0.02)
                #expect(abs(editorAfter.blueComponent - editorBefore.blueComponent) < 0.02)
                #expect(first.textView.hasMarkedText() && first.textView.markedRange() == marked)
                #expect(Array(first.textView.string.utf8) == nativeBytes)
                #expect(first.window?.firstResponder === first.textView)
                #expect(first.model.content == firstContent && second.model.content == secondContent)
                #expect(first.textView.undoManager === firstManager && firstManager.canUndo == firstUndo)
                #expect(second.textView.undoManager === secondManager && secondManager.canUndo == secondUndo)
                #expect(firstManager.canRedo == firstRedo && secondManager.canRedo == secondRedo)
                #expect(announcements.isEmpty)
                first.window?.contentView?.layoutSubtreeIfNeeded()
                let popupRect = first.identityControls.popup.convert(
                    first.identityControls.popup.bounds, to: nil)
                let layout = try #require(first.window?.contentLayoutRect)
                #expect(popupRect.maxY <= layout.maxY + 1)
                #expect(await cue.setMode(.standard))
                try #require(
                    await identityEventually {
                        first.window?.styleMask.contains(.fullSizeContentView) == false
                            && second.window?.styleMask.contains(.fullSizeContentView) == false
                    })
                #expect(first.window?.titlebarAppearsTransparent == false)
                #expect(second.window?.titlebarAppearsTransparent == false)
                #expect(first.textView.hasMarkedText() && first.textView.markedRange() == marked)
                #expect(first.model.content == firstContent && second.model.content == secondContent)
                #expect(first.textView.undoManager === firstManager && firstManager.canUndo == firstUndo)
                #expect(firstManager.canRedo == firstRedo)
                #expect(announcements.isEmpty)
                first.textView.unmarkText()
            }
        }
    }

    @Test func strongChromePreservesExistingNativeUndoAndRedo() async throws {
        let cue = ComposeCueModel(source: NativeCueSource())
        await cue.reload()
        let appearance = AppearanceModel(source: NativeStripAppearanceSource())
        await appearance.refresh()
        try await withNativeIdentity(.saved(nativeContent(body: "")), appearance: appearance, cue: cue) {
            controller, _ in
            try #require(
                await identityEventually {
                    controller.identityStrip.strip.accessibilityLabel()?.contains("a@EXAMPLE.test") == true
                })
            controller.window?.makeFirstResponder(controller.textView)
            controller.textView.insertText("Typed", replacementRange: .init(location: 0, length: 0))
            #expect(await controller.saveAndWait())
            let undo = try #require(controller.textView.undoManager)
            try #require(undo.canUndo)
            #expect(await cue.setMode(.strong))
            try #require(
                await identityEventually {
                    controller.window?.styleMask.contains(.fullSizeContentView) == true
                })
            #expect(undo.canUndo)
            undo.undo()
            #expect(controller.textView.string.isEmpty)
            try #require(undo.canRedo)
            undo.redo()
            #expect(controller.textView.string == "Typed")
        }
    }

    @Test func strongCueReportsLoadingWhileIdentityDiscoveryIsHeld() async throws {
        let cue = ComposeCueModel(source: NativeCueSource())
        await cue.reload()
        #expect(await cue.setMode(.strong))
        let appearance = AppearanceModel(source: NativeStripAppearanceSource())
        await appearance.refresh()
        let gate = NativeIdentityGate()
        try await withNativeIdentity(catalogGate: gate, appearance: appearance, cue: cue) {
            controller, source in
            try #require(await identityEventually { await source.catalogReads == 1 })
            #expect(controller.cueStatusLabel.stringValue == "Loading compose appearance…")
            #expect(!controller.cueStatusLabel.isHidden)
            #expect(controller.identityStrip.strip.accessibilityLabel() == "Loading sending identity…")
            #expect(controller.identityControls.popup.selectedItem?.title == "Loading sending identity…")
            await gate.open()
            try #require(
                await identityEventually {
                    controller.window?.styleMask.contains(.fullSizeContentView) == true
                        && controller.cueStatusLabel.isHidden
                })
        }
    }

    @Test func strongCueReportsLoadingWhileAppearanceIsUnresolved() async throws {
        let cue = ComposeCueModel(source: NativeCueSource())
        await cue.reload()
        #expect(await cue.setMode(.strong))
        let appearance = AppearanceModel(source: NativeStripAppearanceSource())
        try await withNativeIdentity(appearance: appearance, cue: cue) { controller, _ in
            try #require(
                await identityEventually {
                    controller.identityControls.popup.itemArray.contains {
                        ($0.representedObject as? String) == "a"
                    }
                })
            #expect(controller.cueStatusLabel.stringValue == "Loading compose appearance…")
            #expect(
                controller.identityStrip.strip.accessibilityLabel()
                    == "Loading appearance: Account a · Native a <a@EXAMPLE.test>")
            await appearance.refresh()
            try #require(
                await identityEventually {
                    controller.window?.styleMask.contains(.fullSizeContentView) == true
                        && controller.cueStatusLabel.isHidden
                })
        }
    }

    @Test func failedAppearanceReadDoesNotStayInLoadingState() async throws {
        let cue = ComposeCueModel(source: NativeCueSource())
        await cue.reload()
        #expect(await cue.setMode(.strong))
        let source = NativeStripAppearanceSource()
        let appearance = AppearanceModel(source: source)
        try await withNativeIdentity(appearance: appearance, cue: cue) { controller, _ in
            try #require(
                await identityEventually {
                    controller.identityStrip.strip.accessibilityLabel()?.hasPrefix("Loading appearance:")
                        == true
                        && controller.cueStatusLabel.stringValue == "Loading compose appearance…"
                })
            await source.setFailure(.unavailable(reason: "synthetic appearance failure"))
            await appearance.refresh()
            try #require(
                await identityEventually {
                    controller.cueStatusLabel.stringValue
                        == "Strong appearance is unavailable for this sending identity."
                        && controller.identityStrip.strip.accessibilityLabel()?.hasPrefix(
                            "Appearance unavailable:") == true
                })
            #expect(controller.window?.styleMask.contains(.fullSizeContentView) == false)
        }
    }

    @Test func failedDraftLoadDoesNotPretendIdentityIsStillLoading() async throws {
        let cue = ComposeCueModel(source: NativeCueSource())
        await cue.reload()
        #expect(await cue.setMode(.strong))
        try await withNativeIdentity(draftFailure: .unavailable(reason: "synthetic draft failure"), cue: cue)
        {
            controller, _ in
            try #require(
                await identityEventually {
                    controller.statusLabel.stringValue.contains("synthetic draft failure")
                })
            #expect(controller.identityStrip.strip.accessibilityLabel() == "Sending identity unavailable")
            #expect(controller.cueStatusLabel.isHidden)
            #expect(controller.window?.styleMask.contains(.fullSizeContentView) == false)
        }
    }

    @Test func failedCueReadIsExplicitAndRetainsAStaleAcknowledgedMode() async throws {
        let source = NativeCueSource()
        await source.setFailure(.unavailable(reason: "synthetic cue failure"))
        let cue = ComposeCueModel(source: source)
        await cue.reload()
        let appearance = AppearanceModel(source: NativeStripAppearanceSource())
        await appearance.refresh()
        try await withNativeIdentity(appearance: appearance, cue: cue) { controller, _ in
            #expect(cue.mode == nil)
            #expect(controller.window?.styleMask.contains(.fullSizeContentView) == false)
            #expect(!controller.cueStatusLabel.isHidden)
            #expect(controller.cueStatusLabel.stringValue.contains("synthetic cue failure"))
            await source.setFailure(nil)
            await cue.reload()
            #expect(await cue.setMode(.strong))
            try #require(
                await identityEventually {
                    controller.window?.styleMask.contains(.fullSizeContentView) == true
                        && controller.cueStatusLabel.isHidden
                })
            await source.setFailure(.unavailable(reason: "synthetic cue failure"))
            await cue.reload()
            try #require(
                await identityEventually {
                    !controller.cueStatusLabel.isHidden
                        && controller.cueStatusLabel.stringValue.contains("synthetic cue failure")
                })
            #expect(cue.mode == .strong)
            #expect(controller.window?.styleMask.contains(.fullSizeContentView) == true)
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
            // The window renders the read state on a later main-actor
            // turn, so the label and popup are polled, not read once.
            let shown = await identityEventually {
                c.statusLabel.stringValue.contains("Scripted catalog failure")
                    && !c.identityControls.popup.isEnabled
            }
            #expect(shown, "\(c.statusLabel.stringValue)")
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
