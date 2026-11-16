import AppKit
import SweetbriarBridge
import UniformTypeIdentifiers

/// The compose window's attachment controls (spec/08 "Attachments",
/// M1.9c2cc): the Attach… button, file drops, one row per attachment
/// and per unfinished or failed publication, and undoable attach and
/// remove on the window's undo stack. Content edits go through
/// `DraftModel`; publication through `DraftAttachmentPublications`.
@MainActor
final class ComposeAttachmentControls: NSObject {
    let attachButton = NSButton(title: "Attach…", target: nil, action: nil)
    /// Attachment rows, then publication rows.
    let list = NSStackView()
    let publications: DraftAttachmentPublications

    private let model: DraftModel
    private weak var window: NSWindow?
    private let admitted: @MainActor () -> Bool
    private let changed: @MainActor () -> Void
    private let breakCoalescing: @MainActor () -> Void
    private var panel: NSOpenPanel?

    /// `admitted` answers whether new attachments may start now: the
    /// editor is editable and archive writes are available.
    /// `breakCoalescing` keeps an attachment edit out of typing's undo.
    init(
        model: DraftModel, publications: DraftAttachmentPublications, window: NSWindow,
        admitted: @escaping @MainActor () -> Bool, changed: @escaping @MainActor () -> Void,
        breakCoalescing: @escaping @MainActor () -> Void
    ) {
        self.model = model
        self.publications = publications
        self.window = window
        self.admitted = admitted
        self.changed = changed
        self.breakCoalescing = breakCoalescing
        super.init()
        attachButton.target = self
        attachButton.action = #selector(chooseFiles)
        attachButton.setAccessibilityIdentifier("composeAttach")
        list.orientation = .vertical
        list.alignment = .leading
        list.spacing = 4
        list.setAccessibilityIdentifier("composeAttachments")
        publications.onAttached = { [weak self] index in self?.didAttach(at: index) }
    }

    private var undoManager: UndoManager? { window?.undoManager }

    /// `-attachPath <path>`: Attach… takes this file instead of asking,
    /// the compose UI test's seam past the open panel (spec/11 "UI").
    private static let attachPathKey = "attachPath"

    /// Pick files in a sheet. Folders can't be chosen.
    @objc func chooseFiles() {
        guard admitted(), panel == nil, let window, window.attachedSheet == nil else { return }
        if let path = UserDefaults.standard.string(forKey: Self.attachPathKey) {
            attach([URL(fileURLWithPath: path)])
            return
        }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = false
        panel.allowsMultipleSelection = true
        panel.resolvesAliases = true
        panel.prompt = "Attach"
        self.panel = panel
        panel.beginSheetModal(for: window) { [weak self] response in
            guard let self else { return }
            self.panel = nil
            // A render while the sheet was open disabled Attach…; one
            // after a cancel enables it again.
            if response == .OK { attach(panel.urls) } else { render() }
        }
    }

    /// Start publishing each file; answers whether any started. Also the
    /// editor's file-drop handler, so dropped files never become text.
    @discardableResult
    func attach(_ urls: [URL]) -> Bool {
        guard admitted() else { return false }
        let started = urls.filter(\.isFileURL).compactMap { publications.attach(file: $0) }
        render()
        return !started.isEmpty
    }

    private enum Action: String {
        case attach = "Attach File"
        case remove = "Remove Attachment"
    }

    private func didAttach(at index: Int) {
        guard let attachment = attachment(at: index) else { return }
        registerUndo(.attach) { $0.remove(at: index, expecting: attachment, as: .attach) }
        changed()
    }

    /// The Remove button.
    func remove(at index: Int) {
        guard let current = attachment(at: index) else { return }
        remove(at: index, expecting: current, as: .remove)
    }

    /// An undo or redo whose attachment is no longer at `index` does
    /// nothing, so a reloaded draft never loses another attachment.
    private func remove(at index: Int, expecting expected: DraftAttachment, as action: Action) {
        guard admitted(), attachment(at: index) == expected,
            let restoration = model.removeAttachment(at: index)
        else { return }
        registerUndo(action) { $0.restore(restoration, as: action) }
        changed()
    }

    private func restore(_ restoration: DraftModel.AttachmentRestoration, as action: Action) {
        guard admitted(), let index = model.restoreAttachment(restoration),
            let attachment = attachment(at: index)
        else { return }
        registerUndo(action) { $0.remove(at: index, expecting: attachment, as: action) }
        changed()
    }

    private func registerUndo(
        _ action: Action, _ handler: @escaping @MainActor (ComposeAttachmentControls) -> Void
    ) {
        guard let undo = undoManager else { return }
        breakCoalescing()
        undo.beginUndoGrouping()
        undo.registerUndo(withTarget: self, handler: handler)
        undo.setActionName(action.rawValue)
        undo.endUndoGrouping()
        breakCoalescing()
    }

    private func attachment(at index: Int) -> DraftAttachment? {
        guard let attachments = model.content?.attachments, attachments.indices.contains(index) else {
            return nil
        }
        return attachments[index]
    }

    /// Fence new publications and forget this window's attachment undo.
    func stop() {
        panel?.cancel(nil)
        undoManager?.removeAllActions(withTarget: self)
        publications.stop()
    }

    /// Fence, then await every begun native publication.
    func stopAndWait() async {
        stop()
        await publications.stopAndWait()
    }

    func render() {
        let editable = admitted()
        attachButton.isEnabled = editable && panel == nil
        var rows: [NSView] = []
        for (index, attachment) in (model.content?.attachments ?? []).enumerated() {
            let name = Self.filename(of: attachment)
            let remove = ActionButton(title: "Remove") { [weak self] in self?.remove(at: index) }
            remove.isEnabled = editable
            remove.setAccessibilityLabel("Remove \(name)")
            remove.setAccessibilityIdentifier("composeRemoveAttachment-\(index)")
            rows.append(row(label: name, button: remove))
        }
        for publication in publications.publications {
            let label: String
            if let failure = publication.failure {
                label = "Couldn't attach \(publication.filename): \(failure.localizedDescription)"
            } else {
                label = "Attaching \(publication.filename)…"
            }
            let id = publication.id
            let dismiss = ActionButton(title: publication.failure == nil ? "Cancel" : "Dismiss") {
                [weak self] in
                self?.publications.dismiss(id)
                self?.render()
            }
            dismiss.setAccessibilityIdentifier("composeDismissAttachment-\(publication.filename)")
            rows.append(row(label: label, button: dismiss))
        }
        list.setViews(rows, in: .top)
        list.isHidden = rows.isEmpty
    }

    private func row(label: String, button: NSButton) -> NSView {
        let text = NSTextField(labelWithString: label)
        text.lineBreakMode = .byTruncatingMiddle
        text.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        button.bezelStyle = .inline
        let row = NSStackView(views: [text, button])
        row.spacing = 8
        return row
    }

    static func filename(of attachment: DraftAttachment) -> String {
        switch attachment {
        case .new(_, let filename, _): return filename
        case .retained(let reference): return reference.filename
        }
    }

    /// File URLs on a drag pasteboard; empty when it carries none.
    static func fileURLs(on pasteboard: NSPasteboard) -> [URL] {
        let urls = pasteboard.readObjects(
            forClasses: [NSURL.self], options: [.urlReadingFileURLsOnly: true])
        return (urls as? [URL]) ?? []
    }
}

/// A button whose action is a closure, for rows rebuilt on each render.
@MainActor
private final class ActionButton: NSButton {
    private let handler: @MainActor () -> Void
    init(title: String, handler: @escaping @MainActor () -> Void) {
        self.handler = handler
        super.init(frame: .zero)
        self.title = title
        target = self
        action = #selector(run)
    }
    required init?(coder: NSCoder) { fatalError("ActionButton is built in code") }
    @objc private func run() { handler() }
}
