// The search strip under test (plan M1.6o): real windows and field
// editors drive the native seams: marked text is never parsed, a
// committed edit submits, the scope/sort popups reach the model
// without folding into `in:` chips, a chip edit selects its
// canonical span without rewriting it, live input is never
// rewritten after a parse, and errors land as UTF-16 underlines
// beside the retained last-valid label. Semantic parser coverage
// lives in the bridge suite; a stub parser feeds the model here.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import Testing
import SweetbriarBridge

private let flagJSON = #"{"v":1,"q":{"flag":"seen"}}"#
private let clauseJSON = #"{"v":1,"q":{"and":[{"mailbox":9},{"text":"hello"}]}}"#

/// A `SearchParsing` fake: controlled values plus call observation.
/// `parse` may suspend (a gate) so mid-parse compositions are testable.
private actor ParserStub: SearchParsing {
    private(set) var parsed: [String] = []
    private(set) var finishedParses = 0
    private let parseBody: @Sendable (String) async throws -> String
    private let formatBody: @Sendable (String) async throws -> String

    init(
        parse: @escaping @Sendable (String) async throws -> String,
        format: @escaping @Sendable (String) async throws -> String
    ) {
        parseBody = parse
        formatBody = format
    }

    func parseSearch(text: String, context: ParseContext) async throws -> String {
        parsed.append(text)
        defer { finishedParses += 1 }
        return try await parseBody(text)
    }

    func formatSearch(queryJSON: String) async throws -> String {
        try await formatBody(queryJSON)
    }
}

private actor Gate {
    private var released = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if released { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func release() {
        released = true
        let pending = waiters
        waiters = []
        for waiter in pending { waiter.resume() }
    }
}

/// Clause renders that literally appear in the whole, as the model
/// requires to locate each clause's range in the canonical field.
private func renderClause(_ json: String) throws -> String {
    if json.contains(#""and""#) { return #"(in:9 "hello")"# }
    if json.contains(#""mailbox""#) { return "in:9" }
    return #""hello""#
}

@Suite("the search field strip", .serialized)
@MainActor
struct SearchFieldTests {
    private func hostedWindow(_ controller: SearchFieldController) -> NSWindow {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 140),
            styleMask: [.titled, .resizable],
            backing: .buffered,
            defer: false
        )
        window.isReleasedWhenClosed = false
        window.contentView = controller.view
        window.makeKeyAndOrderFront(nil)
        return window
    }

    private func find(in view: NSView, identifier: String) -> NSView? {
        if view.accessibilityIdentifier() == identifier { return view }
        for subview in view.subviews {
            if let found = find(in: subview, identifier: identifier) { return found }
        }
        return nil
    }

    private func findAll(in view: NSView, prefix: String) -> [NSView] {
        var found: [NSView] = []
        if view.accessibilityIdentifier()?.hasPrefix(prefix) == true { found.append(view) }
        for subview in view.subviews { found += findAll(in: subview, prefix: prefix) }
        return found
    }

    /// Poll until the condition holds or the limit passes; a broken
    /// strip fails in seconds instead of hanging the suite.
    private func until(_ limit: Duration, _ condition: () async -> Bool) async -> Bool {
        let deadline = ContinuousClock.now + limit
        while true {
            if await condition() { return true }
            if ContinuousClock.now >= deadline { return false }
            try? await Task.sleep(for: .milliseconds(10))
        }
    }

    @Test("marked text is never parsed, even inside the debounce; committing submits")
    func markedTextGating() async throws {
        let parser = ParserStub(parse: { _ in flagJSON }, format: { _ in "is:read" })
        let model = SearchFieldModel(parser: parser)
        let controller = SearchFieldController(model: model, scopes: { [] })
        let window = hostedWindow(controller)
        defer { model.close(); window.close() }
        let field = try #require(find(in: controller.view, identifier: "search.field") as? NSSearchField)
        try #require(window.makeFirstResponder(field))
        let editor = try #require(field.currentEditor() as? NSTextView)

        // A committed keystroke starts the 30 ms debounce; a
        // composition starting inside it cancels the pending parse.
        editor.insertText("a", replacementRange: NSRange(location: 0, length: 0))
        editor.setMarkedText(
            "に", selectedRange: NSRange(location: 0, length: 1),
            replacementRange: NSRange(location: 1, length: 0)
        )
        #expect(editor.hasMarkedText())
        try await Task.sleep(for: .milliseconds(80))
        #expect(await parser.parsed.isEmpty)

        editor.insertText("日", replacementRange: NSRange(location: 1, length: 1))
        #expect(await until(.seconds(2)) { await parser.parsed == ["a日"] })
        #expect(editor.string == "a日")
        #expect(model.text == "a日")
    }

    @Test("a composition started mid-parse discards that parse")
    func midParseComposition() async throws {
        let gate = Gate()
        let parser = ParserStub(
            parse: { _ in await gate.wait(); return flagJSON },
            format: { _ in "is:read" }
        )
        let model = SearchFieldModel(parser: parser)
        let controller = SearchFieldController(model: model, scopes: { [] })
        let window = hostedWindow(controller)
        defer { model.close(); window.close() }
        let field = try #require(find(in: controller.view, identifier: "search.field") as? NSSearchField)
        try #require(window.makeFirstResponder(field))
        let editor = try #require(field.currentEditor() as? NSTextView)

        editor.insertText("a", replacementRange: NSRange(location: 0, length: 0))
        #expect(await until(.seconds(2)) { await parser.parsed == ["a"] })
        editor.setMarkedText(
            "に", selectedRange: NSRange(location: 0, length: 1),
            replacementRange: NSRange(location: 1, length: 0)
        )
        await gate.release()
        #expect(await until(.seconds(2)) { await parser.finishedParses == 1 })
        #expect(model.queryJSON == nil)
        #expect(model.failure == nil)
        #expect(editor.string == "aに")
    }

    @Test("scope and sort controls reach the model, apart from membership chips")
    func scopeSortAndDistinct() async throws {
        let parser = ParserStub(parse: { _ in clauseJSON }, format: renderClause)
        var scopeEntries: [(label: String, scope: SearchScope)] = [
            ("Archive", .mailbox(9)), ("Work", .account("work")),
        ]
        let model = SearchFieldModel(parser: parser, label: { $0 == .mailbox(9) ? "in:Archive" : nil })
        let controller = SearchFieldController(model: model, scopes: { scopeEntries })
        let window = hostedWindow(controller)
        defer { model.close(); window.close() }
        let field = try #require(find(in: controller.view, identifier: "search.field") as? NSSearchField)
        try #require(window.makeFirstResponder(field))
        let editor = try #require(field.currentEditor() as? NSTextView)
        editor.insertText("in:9 hello", replacementRange: NSRange(location: 0, length: 0))
        #expect(await until(.seconds(2)) { model.canEditClauses })

        let scopePopup = try #require(
            find(in: controller.view, identifier: "search.scope") as? NSPopUpButton
        )
        let archive = try #require(scopePopup.itemArray.first { $0.title == "Archive" })
        #expect(NSApp.sendAction(try #require(archive.action), to: archive.target, from: archive))
        #expect(model.scope == .mailbox(9))
        // The visible scope and the `in:` chip are separate controls.
        #expect(await until(.seconds(2)) { scopePopup.selectedItem?.title == "Archive" })
        #expect(await until(.seconds(2)) {
            self.findAll(in: controller.view, prefix: "search.clause.")
                .compactMap { ($0 as? NSButton)?.title } == ["in:Archive"]
        })

        let sortPopup = try #require(
            find(in: controller.view, identifier: "search.sort") as? NSPopUpButton
        )
        sortPopup.selectItem(withTitle: "Date")
        #expect(NSApp.sendAction(try #require(sortPopup.action), to: sortPopup.target, from: sortPopup))
        #expect(model.sort == .date)

        // Late sidebar loads refresh the options on the next open.
        scopeEntries.append(("Other", .account("other")))
        scopePopup.menu?.update()
        model.chooseSort(.relevance) // any tracked change repaints too
        #expect(await until(.seconds(2)) {
            scopePopup.itemArray.map(\.title) == ["All accounts", "Archive", "Work", "Other"]
        })

        // A missing name retains the chosen filter; only an explicit reset broadens it.
        scopeEntries.removeAll { $0.scope == .mailbox(9) }
        scopePopup.menu?.update()
        model.chooseSort(.date)
        #expect(await until(.seconds(2)) { scopePopup.selectedItem?.title == "Mailbox 9" })
        #expect(model.scope == .mailbox(9))
        let all = try #require(scopePopup.itemArray.first { $0.title == "All accounts" })
        #expect(NSApp.sendAction(try #require(all.action), to: all.target, from: all))
        #expect(model.scope == .all)
        #expect(model.clauses.count == 2)
    }

    @Test("blur commits composition; list navigation retires the marked buffer")
    func compositionEndAndNavigation() async throws {
        let parser = ParserStub(parse: { _ in flagJSON }, format: { _ in "is:read" })
        let model = SearchFieldModel(parser: parser)
        let controller = SearchFieldController(model: model, scopes: { [] })
        let window = hostedWindow(controller)
        defer { model.close(); window.close() }
        let field = try #require(find(in: controller.view, identifier: "search.field") as? NSSearchField)
        try #require(window.makeFirstResponder(field))
        let editor = try #require(field.currentEditor() as? NSTextView)
        editor.setMarkedText("に", selectedRange: NSRange(location: 1, length: 0),
                             replacementRange: NSRange(location: 0, length: 0))
        #expect(editor.hasMarkedText())
        editor.unmarkText()
        try #require(window.makeFirstResponder(nil))
        #expect(await until(.seconds(2)) { await parser.parsed == ["に"] })
        try #require(window.makeFirstResponder(field))
        let resumed = try #require(field.currentEditor() as? NSTextView)
        resumed.setMarkedText("ほ", selectedRange: NSRange(location: 1, length: 0),
                             replacementRange: NSRange(location: 1, length: 0))
        #expect(resumed.hasMarkedText())
        controller.navigated(to: .flagged)
        #expect(!resumed.hasMarkedText() && model.text.isEmpty && field.stringValue.isEmpty)
        // A late end-editing event cannot resurrect the prior buffer.
        NotificationCenter.default.post(name: NSControl.textDidEndEditingNotification, object: field)
        try await Task.sleep(for: .milliseconds(80))
        #expect(model.queryJSON == nil && model.text.isEmpty)
        #expect(await parser.parsed == ["に"])
    }

    @Test("chip edit focuses the editor and selects the canonical span")
    func chipEditSelects() async throws {
        let parser = ParserStub(parse: { _ in clauseJSON }, format: renderClause)
        let model = SearchFieldModel(parser: parser, label: { $0 == .mailbox(9) ? "Archive" : nil })
        let controller = SearchFieldController(model: model, scopes: { [] })
        let window = hostedWindow(controller)
        defer { model.close(); window.close() }
        let field = try #require(find(in: controller.view, identifier: "search.field") as? NSSearchField)
        try #require(window.makeFirstResponder(field))
        let editor = try #require(field.currentEditor() as? NSTextView)
        editor.insertText("in:9 hello", replacementRange: NSRange(location: 0, length: 0))
        #expect(await until(.seconds(2)) { model.canEditClauses })
        let clause = try #require(model.clauses.first { $0.isChip })
        let chipID = "search.clause.\(clause.id.uuidString)"
        #expect(await until(.seconds(2)) { self.find(in: controller.view, identifier: chipID) != nil })

        let chip = try #require(find(in: controller.view, identifier: chipID) as? NSButton)
        #expect(chip.isEnabled)
        chip.performClick(nil)
        #expect(editor.string == #"(in:9 "hello")"#)
        #expect(window.firstResponder === editor)
        #expect(editor.selectedRange() == NSRange(location: 1, length: 4))
        // Submit the canonical field without rewriting its semantics.
        #expect(await until(.seconds(2)) { await parser.parsed.last == #"(in:9 "hello")"# })
    }

    @Test("live input is not rewritten; errors mark the UTF-16 span and keep the last-valid label")
    func liveInputAndErrors() async throws {
        let parser = ParserStub(parse: { text in
            switch text {
            case "bad é": throw CoreFailure.searchParse(start: 4, end: 6, code: .invalidValue)
            case "end": throw CoreFailure.searchParse(start: 3, end: 3, code: .missingValue)
            default: return flagJSON
            }
        }, format: { _ in "is:read" })
        let model = SearchFieldModel(parser: parser)
        let controller = SearchFieldController(model: model, scopes: { [] })
        let window = hostedWindow(controller)
        defer { model.close(); window.close() }
        let field = try #require(find(in: controller.view, identifier: "search.field") as? NSSearchField)
        try #require(window.makeFirstResponder(field))
        let editor = try #require(field.currentEditor() as? NSTextView)
        let status = try #require(find(in: controller.view, identifier: "search.status") as? NSTextField)
        let storage = try #require(editor.textStorage)
        let manager = try #require(editor.layoutManager)

        // Canonical "is:read" never replaces the raw live input.
        editor.insertText("  is:read  ", replacementRange: NSRange(location: 0, length: 0))
        #expect(await until(.seconds(2)) { model.queryJSON != nil })
        #expect(editor.string == "  is:read  ")
        #expect(status.isHidden)

        // é is two UTF-8 bytes: [4,6) maps to UTF-16 (4,1).
        editor.insertText("bad é", replacementRange: NSRange(location: 0, length: 11))
        #expect(await until(.seconds(2)) {
            status.stringValue.contains("bytes [4,6)") &&
                manager.temporaryAttribute(.underlineColor, atCharacterIndex: 4, effectiveRange: nil) as? NSColor == .systemRed
        })
        #expect(storage.attribute(.underlineColor, at: 4, effectiveRange: nil) == nil)
        #expect(storage.attribute(.underlineColor, at: 0, effectiveRange: nil) == nil)
        #expect(status.stringValue.contains("bytes [4,6)"))
        #expect(status.stringValue.contains("Showing results for:"))

        // A zero-length EOF span has nothing to underline; the label
        // still makes the error visible.
        editor.insertText("end", replacementRange: NSRange(location: 0, length: 5))
        #expect(await until(.seconds(2)) { status.stringValue.contains("bytes [3,3)") })
        #expect(manager.temporaryAttribute(.underlineColor, atCharacterIndex: 0, effectiveRange: nil) == nil)
        #expect(manager.temporaryAttribute(.underlineColor, atCharacterIndex: 2, effectiveRange: nil) == nil)
        #expect(storage.attribute(.underlineColor, at: 0, effectiveRange: nil) == nil)
        #expect(storage.attribute(.underlineColor, at: 2, effectiveRange: nil) == nil)
    }
}
#endif
