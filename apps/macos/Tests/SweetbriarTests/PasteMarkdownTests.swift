import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

/// Answers each conversion with the next scripted result.
private final class ScriptedPasteSource: PasteConversionSource, @unchecked Sendable {
    private let lock = NSLock()
    private var answers: [Result<MarkdownPaste, CoreFailure>]
    private(set) var requests: [String] = []
    init(_ answers: [Result<MarkdownPaste, CoreFailure>]) { self.answers = answers }
    func markdownFromHtml(html: String) async throws -> MarkdownPaste {
        let answer = lock.withLock {
            requests.append(html)
            return answers.removeFirst()
        }
        return try answer.get()
    }
}

@MainActor
private func pasteEventually(_ predicate: @MainActor () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !predicate() {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

/// A Markdown draft holding `body`, open in a window whose editor has
/// focus and its caret at the end, over a private pasteboard with `html`.
@MainActor
private func withPasteWindow(
    body: String, html: String?, answers: [Result<MarkdownPaste, CoreFailure>],
    test: @MainActor (ComposeWindowController, ScriptedPasteSource, NSPasteboard) async throws -> Void
) async throws {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("sb-paste-\(UUID().uuidString)")
    let bridge = try await CoreBridge.open(
        config: CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path),
        snapshots: FrameBatcher<[Setting]>(clock: PasteClock()) { _ in })
    let pasteboard = NSPasteboard(name: NSPasteboard.Name("sb-paste-\(UUID().uuidString)"))
    pasteboard.clearContents()
    if let html { pasteboard.setString(html, forType: .html) }
    pasteboard.setString("plain fallback", forType: .string)
    let source = ScriptedPasteSource(answers)
    var controller: ComposeWindowController?
    let result: Result<Void, any Error>
    do {
        let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: created.draftId, baseRevisionId: nil,
            content: DraftContent(
                identityId: nil, format: .markdown, plainFlowed: true, subject: "Paste",
                to: [], cc: [], bcc: [], bodySource: body, attachments: []))
        let window = ComposeWindowController(
            source: bridge, draftId: created.draftId, pasteSource: source, pasteboard: pasteboard)
        controller = window
        window.showWindow(nil)
        try #require(
            await pasteEventually {
                window.textView.isEditable && window.textView.string == body && window.textView.mode == .markdown
            })
        window.window?.makeFirstResponder(window.textView)
        window.textView.setSelectedRange(NSRange(location: (body as NSString).length, length: 0))
        try await test(window, source, pasteboard)
        result = .success(())
    } catch { result = .failure(error) }
    if let controller, !(await controller.closeAndWait()) {
        _ = await controller.closeWithoutSavingAndWait()
    }
    pasteboard.releaseGlobally()
    await bridge.shutdown()
    try? FileManager.default.removeItem(at: root)
    try result.get()
}

private struct PasteClock: FrameClock { func nextFrame() async { await Task.yield() } }

@Suite(.serialized, .timeLimit(.minutes(1))) @MainActor
struct PasteMarkdownTests {
    @Test func lossPhrasesFollowTheCoreOrderAndLosslessSaysNothing() {
        #expect(PasteMarkdownSheet.lossMessage([]) == nil)
        #expect(
            PasteMarkdownSheet.lossMessage([.images, .styling])
                == "Some formatting can't be kept: images, text styles and colors.")
        #expect(
            PasteMarkdownSheet.lossMessage([.links, .tables, .other])
                == "Some formatting can't be kept: some links, table layout, other content.")
    }

    @Test func theMenuOffersPasteAsMarkdownOnOptionCommandV() throws {
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {})
        let edit = try #require(builder.makeMainMenu().items.compactMap(\.submenu).first { $0.title == "Edit" })
        let item = try #require(edit.items.first { $0.action == #selector(ComposeWindowController.pasteAsMarkdown(_:)) })
        #expect(item.keyEquivalent == "v")
        #expect(item.keyEquivalentModifierMask == [.option, .command])
        let paste = try #require(edit.items.first { $0.action == #selector(NSText.paste(_:)) })
        #expect(paste.keyEquivalentModifierMask == [.command])
    }

    @Test func confirmingInsertsTheCapturedConversionAsOneUndoableEdit() async throws {
        let converted = MarkdownPaste(markdown: "**bold** text", losses: [])
        try await withPasteWindow(body: "Hi ", html: "<b>bold</b> text", answers: [.success(converted)]) {
            controller, source, pasteboard in
            #expect(controller.canPasteAsMarkdown)
            controller.pasteAsMarkdown(nil)
            let sheet = try #require(controller.pasteSheet)
            #expect(!controller.canPasteAsMarkdown, "one sheet at a time")
            try #require(await pasteEventually { sheet.state == .ready(converted) })
            #expect(sheet.preview.string == "**bold** text")
            #expect(sheet.messageLabel.isHidden)
            #expect(sheet.pasteButton.isEnabled)
            // The sheet keeps what it captured.
            pasteboard.clearContents()
            pasteboard.setString("<i>other</i>", forType: .html)
            #expect(sheet.preview.string == "**bold** text")
            sheet.paste(nil)
            #expect(controller.pasteSheet == nil)
            #expect(controller.textView.string == "Hi **bold** text")
            #expect(source.requests == ["<b>bold</b> text"])
            let undo = try #require(controller.textView.undoManager)
            #expect(undo.undoActionName == "Paste as Markdown")
            undo.undo()
            #expect(controller.textView.string == "Hi ")
        }
    }

    @Test func aLossyConversionWarnsAndCancelChangesNothing() async throws {
        let converted = MarkdownPaste(markdown: "a", losses: [.images, .styling])
        try await withPasteWindow(body: "Hi", html: "<p style=color:red>a<img src=x></p>", answers: [.success(converted)]) {
            controller, _, _ in
            controller.pasteAsMarkdown(nil)
            let sheet = try #require(controller.pasteSheet)
            try #require(await pasteEventually { sheet.state == .ready(converted) })
            #expect(!sheet.messageLabel.isHidden)
            #expect(sheet.messageLabel.stringValue == "Some formatting can't be kept: images, text styles and colors.")
            sheet.cancel(nil)
            #expect(controller.pasteSheet == nil)
            #expect(controller.textView.string == "Hi")
        }
    }

    @Test func aRefusalShowsTheReasonWithOnlyCancel() async throws {
        let refusal = CoreFailure.invalidInput(field: "html", message: "too large: use at most 2 MiB of UTF-8")
        try await withPasteWindow(body: "Hi", html: "<p>a</p>", answers: [.failure(refusal)]) { controller, _, _ in
            controller.pasteAsMarkdown(nil)
            let sheet = try #require(controller.pasteSheet)
            try #require(await pasteEventually { if case .refused = sheet.state { true } else { false } })
            #expect(sheet.pasteButton.isHidden)
            #expect(sheet.messageLabel.stringValue.contains("too large"))
            sheet.paste(nil)
            #expect(controller.pasteSheet != nil, "Paste can't act on a refusal")
            sheet.cancel(nil)
            #expect(controller.textView.string == "Hi")
        }
    }

    @Test func itIsDisabledWithoutHtmlInPlainModeOrWithoutFocus() async throws {
        try await withPasteWindow(body: "Hi", html: nil, answers: []) { controller, _, pasteboard in
            #expect(!controller.canPasteAsMarkdown, "no HTML on the pasteboard")
            pasteboard.setString("<b>x</b>", forType: .html)
            #expect(controller.canPasteAsMarkdown)
            controller.textView.mode = .plain
            #expect(!controller.canPasteAsMarkdown, "a plain draft")
            controller.textView.mode = .markdown
            controller.window?.makeFirstResponder(controller.subjectField)
            #expect(!controller.canPasteAsMarkdown, "the body editor lacks focus")
            let item = NSMenuItem(
                title: "", action: #selector(ComposeWindowController.pasteAsMarkdown(_:)), keyEquivalent: "")
            #expect(!controller.validateMenuItem(item))
        }
    }

    @Test func closingTheWindowDismissesAnOpenSheetWithoutInserting() async throws {
        try await withPasteWindow(body: "Hi", html: "<b>x</b>", answers: [.success(MarkdownPaste(markdown: "**x**", losses: []))]) {
            controller, _, _ in
            controller.pasteAsMarkdown(nil)
            let sheet = try #require(controller.pasteSheet)
            #expect(await controller.closeAndWait())
            #expect(controller.pasteSheet == nil)
            #expect(sheet.window.sheetParent == nil)
        }
    }
}
