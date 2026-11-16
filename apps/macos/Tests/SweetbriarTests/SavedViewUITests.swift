// The saved-view and reader-metadata surfaces (spec/08): the sidebar's
// views, the tag picker and the note editor.

import AppKit
import Testing

@testable import SweetbriarBridge

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

/// A scripted `SidebarSource` with saved views.
private actor ViewSidebarSource: SidebarSource {
    var views: [SavedView] = []

    func setViews(_ views: [SavedView]) { self.views = views }

    func accounts() async throws -> [Account] { [] }

    func mailboxes(accountId: String) async throws -> Mailboxes { Mailboxes(generation: 0, mailboxes: []) }

    func savedViews() async throws -> [SavedView] { views }

    func upsertSavedView(idempotencyKey: String, params: SavedViewParams) async throws -> Receipt {
        let id = params.viewId ?? "00000000-0000-4000-8000-000000000001"
        let view = SavedView(
            viewId: id, name: params.name, queryJson: params.queryJson, sort: params.sort, revision: 1
        )
        views.removeAll { $0.viewId == id }
        views.append(view)
        return Receipt(command: "upsert_saved_view", resultJson: "{\"view_id\":\"\(id)\"}", createdAtMs: 0)
    }

    func deleteSavedView(idempotencyKey: String, viewId: String) async throws -> Receipt {
        views.removeAll { $0.viewId == viewId }
        return Receipt(command: "delete_saved_view", resultJson: "{\"view_id\":\"\(viewId)\"}", createdAtMs: 0)
    }
}

private let savedView = SavedView(
    viewId: "00000000-0000-4000-8000-000000000001", name: "Saved search",
    queryJson: #"{"v":1,"q":{"text":"hello"}}"#, sort: .relevance, revision: 1
)

/// A parser stub that always answers the same query.
private actor StubSearchParser: SearchParsing {
    func parseSearch(text: String, context: ParseContext) async throws -> String { savedView.queryJson }

    func formatSearch(queryJSON: String) async throws -> String { #""hello""# }
}

/// A `ReaderSource` with mutable metadata and recorded commands.
/// A class, not an actor: `releaseLease` is synchronous.
private final class MutableReaderSource: ReaderSource, @unchecked Sendable {
    private let lock = NSLock()
    private var message: ReaderMessage
    private let path: String
    private var added: [String] = []
    private var removed: [String] = []
    private var notes: [String] = []
    private var targets: [Int64] = []
    private var selections: [MessageSelection] = []
    var addError: (any Error)?
    var noteError: (any Error)?

    var addedCommands: [String] { lock.withLock { added } }
    var removedCommands: [String] { lock.withLock { removed } }
    var noteCommands: [String] { lock.withLock { notes } }
    var noteTargets: [Int64] { lock.withLock { targets } }
    var recordedSelections: [MessageSelection] { lock.withLock { selections } }

    init(path: String) {
        self.path = path
        message = ReaderMessage(
            messageId: 7,
            headers: .init(from: [], to: [], cc: [], date: nil, subject: "Message", messageId: nil),
            accountId: "a1", accountDisplayName: "Local Archive", occurrences: [],
            threadId: nil, tags: [], note: nil, bodyAvailability: .acquired
        )
    }

    func messageForOccurrence(occurrenceId: Int64) async throws -> Int64 { occurrenceId == 43 ? 8 : 7 }

    func getMessage(messageId: Int64) async throws -> ReaderMessage {
        var other = lock.withLock { message }
        if messageId != other.messageId { (other.messageId, other.tags, other.note) = (messageId, [], nil) }
        return other
    }

    func openBody(messageID: Int64, preferred: BodyKind) async throws -> BodyLease {
        BodyLease(
            leaseId: "body", kind: .plainText, filePath: path, byteLength: 0,
            remoteContentBlocked: 0, warnings: [], manifestPath: nil
        )
    }

    func openAttachment(messageID: Int64, partID: String) async throws -> AttachmentLease {
        AttachmentLease(
            leaseId: "attachment", filePath: path, byteLength: 0, partId: partID,
            displayName: "File", mediaType: "text/plain"
        )
    }

    func releaseLease(_ leaseID: String) {}

    func addTag(idempotencyKey: String, selection: MessageSelection, tag: String) async throws -> Receipt {
        let outcome: Result<Receipt, any Error> = lock.withLock {
            if let error = addError { return .failure(error) }
            added.append(tag)
            selections.append(selection)
            message.tags.append(ReaderTag(tagId: "tag-\(tag)", name: tag, colorSrgb: nil))
            return .success(Receipt(command: "add_tag", resultJson: "{}", createdAtMs: 0))
        }
        return try outcome.get()
    }

    func removeTag(idempotencyKey: String, selection: MessageSelection, tag: String) async throws -> Receipt {
        let outcome: Result<Receipt, any Error> = lock.withLock {
            removed.append(tag)
            selections.append(selection)
            message.tags.removeAll { $0.name == tag }
            return .success(Receipt(command: "remove_tag", resultJson: "{}", createdAtMs: 0))
        }
        return try outcome.get()
    }

    func setNote(idempotencyKey: String, messageId: Int64, text: String) async throws -> Receipt {
        let outcome: Result<Receipt, any Error> = lock.withLock {
            if let error = noteError { return .failure(error) }
            notes.append(text)
            targets.append(messageId)
            if messageId == message.messageId { message.note = text.isEmpty ? nil : text }
            return .success(Receipt(command: "set_note", resultJson: "{}", createdAtMs: 0))
        }
        return try outcome.get()
    }
}

private func readerFile() throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    try Data("Body".utf8).write(to: url)
    return url
}

/// A view that reliably takes first responder, ending the note's
/// editing session.
private final class FocusSink: NSView {
    override var acceptsFirstResponder: Bool { true }
}

@MainActor
private func makeSidebar(_ model: SidebarModel) -> SidebarViewController {
    let sidebar = SidebarViewController(model: model, appearance: AppearanceModel(source: SidebarAppearanceSource()))
    _ = sidebar.view
    return sidebar
}

@MainActor
private func find(in view: NSView, id: String) -> NSView? {
    if view.accessibilityIdentifier() == id { return view }
    for subview in view.subviews {
        if let found = find(in: subview, id: id) { return found }
    }
    return nil
}

@MainActor
private func waitUntil(_ limit: Duration = .seconds(5), _ condition: () async -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + limit
    while true {
        if await condition() { return true }
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(10))
    }
}

@Suite("Saved views and reader metadata")
@MainActor
struct SavedViewUITests {
    /// The search strip's command saves through the sidebar model and
    /// selects the new view, which opens its window.
    @Test func savingASearchListsAViewAndSelectingItOpensItsWindow() async throws {
        let source = ViewSidebarSource()
        let sidebarModel = SidebarModel(source: source)
        await sidebarModel.reload()
        let sidebar = makeSidebar(sidebarModel)
        var chosen: WindowSpec?
        sidebar.onChoose = { chosen = $0 }

        let searchModel = SearchFieldModel(parser: StubSearchParser())
        let search = SearchFieldController(model: searchModel, scopes: { [] })
        _ = search.view
        search.promptForName = { confirm in confirm("Saved search") }
        search.onSaveAsView = { name in
            let view = try await sidebarModel.saveView(
                name: name, queryJSON: searchModel.queryJSON ?? "", sort: searchModel.sort.savedViewSort
            )
            sidebar.reloadSections()
            sidebar.choose(entryID: SidebarModel.savedViewEntryID(view.viewId))
        }

        searchModel.updateText(#""hello""#)
        #expect(await waitUntil { searchModel.queryJSON != nil })
        let save = try #require(find(in: search.view, id: "search.saveView") as? NSButton)
        #expect(save.isEnabled)
        save.performClick(nil)

        #expect(await waitUntil { chosen != nil })
        let viewId = try #require(await source.views.first?.viewId)
        #expect(chosen == .view(viewId))
        let entry = sidebarModel.entry(id: SidebarModel.savedViewEntryID(viewId))
        #expect(entry?.target == .window(.view(viewId)))
        #expect(sidebarModel.savedView(id: viewId)?.name == "Saved search")
    }

    /// Rename and Delete are the saved view's context-menu commands;
    /// delete removes the row and rename keeps the query and sort.
    @Test func renameAndDeleteGoThroughTheSavedViewContextMenu() async throws {
        let source = ViewSidebarSource()
        await source.setViews([savedView])
        let sidebarModel = SidebarModel(source: source)
        await sidebarModel.reload()
        let sidebar = makeSidebar(sidebarModel)
        sidebar.reloadSections()
        sidebar.promptName = { confirm in confirm("Renamed") }
        sidebar.confirmDelete = { _, confirm in confirm(true) }
        #expect(sidebar.contextMenu.items.map(\.title) == ["Rename…", "Delete"])
        #expect(sidebar.contextMenu.items.allSatisfy { $0.target === sidebar } && !sidebar.contextMenu.autoenablesItems)

        sidebar.renameSavedView(id: savedView.viewId)
        #expect(await waitUntil { await source.views.first?.name == "Renamed" })
        #expect(sidebarModel.savedViews.first?.name == "Renamed")
        #expect(sidebarModel.savedViews.first?.queryJson == savedView.queryJson)

        sidebar.deleteSavedView(id: savedView.viewId)
        #expect(await waitUntil { await source.views.isEmpty })
        #expect(sidebarModel.savedViews.isEmpty)
        #expect(sidebarModel.entry(id: SidebarModel.savedViewEntryID(savedView.viewId)) == nil)
    }

    /// The tag picker applies a typed name and removes a chip.
    @Test func theTagPickerAppliesAndRemovesATag() async throws {
        let file = try readerFile()
        defer { try? FileManager.default.removeItem(at: file) }
        let source = MutableReaderSource(path: file.path)
        let controller = ReaderViewController(model: ReaderModel(source: source))
        await controller.show(selection: [42])
        #expect(controller.tagPicker.tagNames.isEmpty)
        #expect((find(in: controller.tagPicker, id: "readerTag.field") as? NSTextField)?.isEditable == true)

        controller.tagPicker.text = "work"
        let add = try #require(find(in: controller.tagPicker, id: "readerTag.add") as? NSButton)
        add.performClick(nil)
        #expect(await waitUntil { !source.addedCommands.isEmpty })
        #expect(source.addedCommands == ["work"])
        #expect(source.recordedSelections == [.occurrenceIds([42])])
        #expect(controller.tagPicker.tagNames == ["work"])

        let chip = try #require(find(in: controller.tagPicker, id: "readerTag.remove.tag-work") as? NSButton)
        chip.performClick(nil)
        #expect(await waitUntil { !source.removedCommands.isEmpty })
        #expect(source.removedCommands == ["work"])
        #expect(controller.tagPicker.tagNames.isEmpty)

        // A failed write shows its error and keeps the shown tags.
        source.addError = CoreFailure.invalidInput(field: "tag", message: "too long")
        controller.tagPicker.text = "again"
        try #require(find(in: controller.tagPicker, id: "readerTag.add") as? NSButton).performClick(nil)
        #expect(await waitUntil { !controller.status.isHidden })
        #expect(controller.status.stringValue.contains("too long"))
        #expect(controller.tagPicker.tagNames.isEmpty)
    }

    /// The note editor saves on end-editing and clears an empty note.
    @Test func theNoteEditorSavesAndClearsOnEndEditing() async throws {
        let file = try readerFile()
        defer { try? FileManager.default.removeItem(at: file) }
        let source = MutableReaderSource(path: file.path)
        let controller = ReaderViewController(model: ReaderModel(source: source))
        await controller.show(selection: [42])
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 480, height: 320),
            styleMask: [.titled, .resizable], backing: .buffered, defer: false
        )
        window.isReleasedWhenClosed = false
        window.contentView = controller.view
        window.makeKeyAndOrderFront(nil)
        defer { window.close() }
        let editor = controller.noteEditor
        let sink = FocusSink()
        sink.translatesAutoresizingMaskIntoConstraints = false
        window.contentView?.addSubview(sink)

        #expect(window.makeFirstResponder(editor.textView))
        #expect(await waitUntil { editor.isEditing })
        editor.textView.string = "a note"
        #expect(window.makeFirstResponder(sink))
        #expect(await waitUntil { !source.noteCommands.isEmpty })
        #expect(source.noteCommands.first == "a note")
        #expect(controller.noteEditor.text == "a note")

        #expect(window.makeFirstResponder(editor.textView))
        #expect(await waitUntil { editor.isEditing })
        editor.textView.string = ""
        #expect(window.makeFirstResponder(sink))
        #expect(await waitUntil { source.noteCommands.count >= 2 })
        #expect(source.noteCommands.last == "")
        #expect(controller.model.currentMessage?.note == nil)
        #expect(controller.noteEditor.text.isEmpty)

        // Clicking another message ends the editing, then selects it in
        // the same turn: the note still saves to the edited message.
        #expect(window.makeFirstResponder(editor.textView))
        editor.textView.string = "b note"
        #expect(window.makeFirstResponder(sink))
        controller.select([43])
        #expect(await waitUntil { source.noteCommands.count >= 3 })
        #expect(source.noteCommands.last == "b note")

        // Navigation mid-edit saves the edit to its message, then shows
        // the next one's note.
        await controller.show(selection: [42])
        #expect(window.makeFirstResponder(editor.textView))
        #expect(await waitUntil { editor.isEditing })
        editor.textView.string = "c note"
        controller.select([43])
        #expect(await waitUntil { controller.model.currentMessage?.messageId == 8 })
        #expect(await waitUntil { source.noteCommands.count >= 4 })
        #expect(source.noteCommands.last == "c note" && editor.text.isEmpty)
        #expect(window.makeFirstResponder(sink))
        #expect(source.noteTargets == [7, 7, 7, 7])
    }
}
