import AppKit
import SweetbriarBridge
import WebKit

@MainActor
final class ReaderViewController: NSViewController {
    let model: ReaderModel
    let header = NSTextField(wrappingLabelWithString: "")
    let accountChip = NSTextField(labelWithString: "")
    let tagPicker = TagPickerView(frame: .zero)
    let noteEditor = NoteEditorView(frame: .zero)
    /// The note last sent: the shown note may lag a write in flight.
    private var noteSent: (messageID: Int64, text: String)?
    /// The message the note editor was filled for, and its note then.
    private var noteDraft: (messageId: Int64, note: String?)?
    let status = NSTextField(wrappingLabelWithString: "No message selected")
    let remoteContent = NSTextField(labelWithString: "")
    let sourceButton = NSButton(title: "View Source", target: nil, action: nil)
    let readingViewToggle = NSButton(checkboxWithTitle: "Reading View", target: nil, action: nil)
    let readingNote = NSTextField(wrappingLabelWithString: "")
    let readingView = ReadingView()
    let attachments = NSStackView()
    let plainText = PlainTextReaderView()
    let textScroll = NSScrollView()
    let readingScroll = NSScrollView()
    private let body = NSView()
    private let details = NSStackView()
    private let reads = ArchiveViewTasks()
    private let memory: ReadingViewMemory
    private let opener: @MainActor (URL) -> Bool
    private let confirmer: @MainActor (URL, [String]) async -> Bool
    private(set) var html: HTMLReaderView!

    init(
        model: ReaderModel,
        opener: @escaping @MainActor (URL) -> Bool = { NSWorkspace.shared.open($0) },
        confirmer: @escaping @MainActor (URL, [String]) async -> Bool = ReaderViewController.confirmLink,
        readResource: @escaping @Sendable (URL) async throws -> Data = { try Data(contentsOf: $0) },
        compileRules: @escaping @MainActor () async throws -> WKContentRuleList = { try await ReaderContentRules.compiled() },
        memory: ReadingViewMemory = ReadingViewMemory(
            source: UnavailableReadingViewMemorySource()
        )
    ) {
        self.model = model
        self.opener = opener
        self.confirmer = confirmer
        self.memory = memory
        model.prefersReadingView = { address in memory.opensReadingView(from: address) }
        super.init(nibName: nil, bundle: nil)
        html = HTMLReaderView(
            currentTicket: { [weak model] in model?.navigationTicket ?? 0 },
            readResource: readResource, compileRules: compileRules
        )
        html.linkActivated = { [weak self] url in
            guard let self else { return }
            let ticket = model.navigationTicket
            reads.run { await self.activateLink(url, ticket: ticket) }
        }
        html.ruleListUnavailable = { [weak self] ticket in
            self?.reads.run { [weak self] in
                guard let self else { return }
                await model.ruleListUnavailable(ticket: ticket)
                await render()
            }
        }
        html.loadFailed = { [weak self] ticket, error in
            guard let self, ticket == model.navigationTicket else { return }
            status.stringValue = "Couldn't display the message: \(error.localizedDescription)"
            status.isHidden = false
        }
        readingView.linkActivated = { [weak self] url in
            guard let self else { return }
            let ticket = model.navigationTicket
            reads.run { await self.activateLink(url, ticket: ticket) }
        }
    }

    required init?(coder: NSCoder) { fatalError("ReaderViewController is built in code") }

    override func loadView() {
        view = NSView()
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        details.orientation = .vertical
        details.alignment = .leading
        details.spacing = 6
        accountChip.textColor = .secondaryLabelColor
        accountChip.drawsBackground = true
        accountChip.backgroundColor = .controlBackgroundColor
        accountChip.setAccessibilityIdentifier("readerAccount")
        header.isSelectable = true
        details.addArrangedSubview(header)
        details.addArrangedSubview(accountChip)
        details.addArrangedSubview(tagPicker)
        details.addArrangedSubview(noteEditor)
        sourceButton.target = self
        sourceButton.action = #selector(toggleSource)
        details.addArrangedSubview(sourceButton)
        readingViewToggle.setAccessibilityIdentifier("readingViewToggle")
        readingViewToggle.target = self
        readingViewToggle.action = #selector(toggleReadingView(_:))
        details.addArrangedSubview(readingViewToggle)
        remoteContent.textColor = .secondaryLabelColor
        remoteContent.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        details.addArrangedSubview(remoteContent)
        attachments.orientation = .vertical
        attachments.alignment = .leading
        details.addArrangedSubview(attachments)
        readingNote.textColor = .secondaryLabelColor
        readingNote.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        readingNote.isHidden = true
        details.addArrangedSubview(readingNote)
        stack.addArrangedSubview(details)
        stack.addArrangedSubview(status)
        stack.addArrangedSubview(body)
        textScroll.hasVerticalScroller = true
        plainText.setAccessibilityIdentifier("readerBody")
        textScroll.documentView = plainText
        plainText.frame = NSRect(x: 0, y: 0, width: 400, height: 0)
        readingScroll.hasVerticalScroller = true
        readingView.setAccessibilityIdentifier("readerReadingView")
        readingScroll.documentView = readingView
        readingView.frame = NSRect(x: 0, y: 0, width: 400, height: 0)
        let panes: [NSView] = [textScroll, html!, readingScroll]
        for pane in panes {
            pane.translatesAutoresizingMaskIntoConstraints = false
            body.addSubview(pane)
            NSLayoutConstraint.activate([
                pane.leadingAnchor.constraint(equalTo: body.leadingAnchor),
                pane.trailingAnchor.constraint(equalTo: body.trailingAnchor),
                pane.topAnchor.constraint(equalTo: body.topAnchor),
                pane.bottomAnchor.constraint(equalTo: body.bottomAnchor),
            ])
        }
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: view.leadingAnchor, constant: 12),
            stack.trailingAnchor.constraint(equalTo: view.trailingAnchor, constant: -12),
            stack.topAnchor.constraint(equalTo: view.safeAreaLayoutGuide.topAnchor, constant: 12),
            stack.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -12),
            body.widthAnchor.constraint(equalTo: stack.widthAnchor),
            header.widthAnchor.constraint(equalTo: stack.widthAnchor),
            details.widthAnchor.constraint(equalTo: stack.widthAnchor),
            tagPicker.widthAnchor.constraint(equalTo: details.widthAnchor),
            noteEditor.widthAnchor.constraint(equalTo: details.widthAnchor),
            noteEditor.heightAnchor.constraint(equalToConstant: 72),
        ])
        body.setContentHuggingPriority(.defaultLow, for: .vertical)
        details.isHidden = true
        textScroll.isHidden = true
        readingScroll.isHidden = true
        html.isHidden = true
        tagPicker.onAdd = { [weak self] name in self?.setTag(name, applied: true) }
        tagPicker.onRemove = { [weak self] tag in self?.setTag(tag.name, applied: false) }
        noteEditor.onEndEditing = { [weak self] text in self?.saveNote(text) }
    }

    @discardableResult
    func select(_ selection: Set<Int64>) -> Task<Void, Never> {
        resetSelection(selection)
        let ticket = model.navigationTicket
        return reads.run { [self] in
            guard ticket == model.navigationTicket else { return }
            await loadSelection(selection)
        }
    }

    func show(selection: Set<Int64>) async {
        resetSelection(selection)
        await loadSelection(selection)
    }

    private func resetSelection(_ selection: Set<Int64>) {
        _ = view
        model.clear()
        html.clear(ticket: model.navigationTicket)
        plainText.text = ""
        readingView.clear()
        updateReadingToggle()
        clearReadingNote()
        details.isHidden = true
        textScroll.isHidden = true
        readingScroll.isHidden = true
        html.isHidden = true
        status.isHidden = false
        guard selection.count == 1 else {
            status.stringValue = selection.isEmpty ? "No message selected" : "\(selection.count) messages selected"
            return
        }
        status.stringValue = "Loading message…"
    }

    private func loadSelection(_ selection: Set<Int64>) async {
        guard selection.count == 1, let id = selection.first else { return }
        await model.show(occurrenceID: id)
        updateReadingToggle()
        clearReadingNote()
        await render()
    }

    @objc private func toggleSource() {
        reads.run { await self.setSource(!self.model.isShowingSource) }
    }

    @objc private func toggleReadingView(_ sender: NSButton) {
        // The state at the click: a render may reset the box before the
        // task runs.
        let enabled = sender.state == .on
        reads.run { await self.setReadingView(enabled) }
    }

    func setSource(_ enabled: Bool) async {
        // Revoke WebKit's frame before requesting another rendition.
        html.isHidden = true
        await model.showSource(enabled)
        await render()
    }

    /// Switch the open message between sender layout and the Reading view,
    /// recording the sender when the list accepts it (spec/08). The toggle
    /// applies to this message even when the write fails.
    func setReadingView(_ enabled: Bool) async {
        guard model.readingDocument != nil else { return }
        // The sender of the message the toggle applies to: reader tasks
        // interleave, so another selection may be current after the render.
        let address = ReaderModel.senderAddress(model.currentMessage)
        let ticket = model.navigationTicket
        model.setReadingView(enabled)
        updateReadingToggle()
        clearReadingNote()
        await render()
        guard let address else { return }
        // The sender is remembered either way; a note about it is only
        // shown while its message is still the one on screen.
        var note: String?
        do {
            if enabled {
                if try await memory.addSender(address) == .listFull {
                    note =
                        "The Reading view sender list is full (\(ReadingViewMemory.limit) senders), "
                        + "so this sender was not remembered. The toggle still applies to this message."
                }
            } else {
                try await memory.removeSender(address)
            }
        } catch {
            note = error.localizedDescription
        }
        if let note, ticket == model.navigationTicket { showReadingNote(note) }
    }

    private func updateReadingToggle() {
        readingViewToggle.state = model.showsReadingView ? .on : .off
        readingViewToggle.isEnabled = model.readingDocument != nil
    }

    private func clearReadingNote() {
        readingNote.stringValue = ""
        readingNote.isHidden = true
    }

    private func showReadingNote(_ text: String) {
        readingNote.stringValue = text
        readingNote.isHidden = false
    }

    func close() {
        reads.retire()
        model.close()
        if isViewLoaded {
            html.clear(ticket: model.navigationTicket)
            plainText.text = ""
            readingView.clear()
        }
    }

    func closeAndWait() async {
        close()
        await reads.waitForRetirement()
    }

    private func render() async {
        if case .empty = model.state { return }
        _ = view
        let ticket = model.navigationTicket
        html.clear(ticket: ticket)
        html.isHidden = true
        textScroll.isHidden = true
        status.isHidden = true
        details.isHidden = model.currentMessage == nil
        if let message = model.currentMessage {
            header.stringValue = Self.headerText(message)
            accountChip.stringValue = "Received in \(message.accountDisplayName)"
            sourceButton.title = model.isShowingSource ? "Sender Layout" : "View Source"
            let blocked = model.currentLease?.remoteContentBlocked ?? 0
            remoteContent.stringValue = "\(blocked) remote content items blocked"
            remoteContent.isHidden = blocked == 0
            renderMetadata(message)
            rebuildAttachments()
        }
        // The Reading view replaces the body rendition while both are
        // kept; source always wins, and a failed load has no message.
        let showReading =
            model.showsReadingView && model.readingDocument != nil && !model.isShowingSource
            && model.currentMessage != nil
        switch model.state {
        case .html(_, let rendition):
            if !showReading {
                html.isHidden = false
                await html.show(rendition, ticket: ticket)
            }
        case .plainText(_, let text):
            if !showReading {
                plainText.mode = .text
                plainText.text = text
                textScroll.isHidden = false
            }
        case .source(_, let text):
            plainText.mode = .source
            plainText.text = text
            textScroll.isHidden = false
        case .unavailable(let error): showError(error)
        case .failed(let error): showError(error)
        case .loading:
            status.stringValue = "Loading message…"
            status.isHidden = false
        case .empty: break
        }
        if showReading, let document = model.readingDocument {
            readingView.show(document)
            readingScroll.isHidden = false
        } else {
            readingScroll.isHidden = true
        }
    }

    static func headerText(_ message: ReaderMessage) -> String {
        func addresses(_ values: [ReaderAddress]) -> String {
            values.map { value in
                switch (value.name, value.address) {
                case (let name?, let address?): "\(name) <\(address)>"
                case (let name?, nil): name
                case (nil, let address?): address
                case (nil, nil): "Unknown"
                }
            }.joined(separator: ", ")
        }
        var lines = ["From: \(addresses(message.headers.from))", "To: \(addresses(message.headers.to))"]
        if let timestamp = message.headers.date {
            let seconds = Duration.milliseconds(timestamp) / .seconds(1)
            lines.append(Date(timeIntervalSince1970: seconds).formatted(date: .abbreviated, time: .shortened))
        }
        return lines.joined(separator: "\n")
    }

    /// Show the message's tags and note; the editor keeps the user's
    /// text while it is being edited.
    private func renderMetadata(_ message: ReaderMessage) {
        tagPicker.configure(tags: message.tags)
        if noteEditor.isEditing {
            // Navigation moved on mid-edit: save to the edited message.
            guard let draft = noteDraft, draft.messageId != message.messageId else { return }
            saveNote(noteEditor.text)
        }
        noteEditor.text = message.note ?? ""
        noteDraft = (message.messageId, message.note)
    }

    /// Apply or remove a tag on the message shown at the click; the
    /// reader re-renders on success and shows the error without
    /// changing the shown tags.
    private func setTag(_ name: String, applied: Bool) {
        guard let occurrenceID = model.occurrenceID, let messageID = model.currentMessage?.messageId
        else { return }
        let ticket = model.navigationTicket
        reads.run { [weak self] in
            guard let self else { return }
            do {
                try await self.model.setTag(
                    name, applied: applied, occurrenceID: occurrenceID, messageID: messageID
                )
                guard ticket == self.model.navigationTicket else { return }
                await self.render()
            } catch {
                guard ticket == self.model.navigationTicket else { return }
                self.showError(error)
            }
        }
    }

    /// Save the note on end-editing; empty text clears it (spec/07).
    /// The message is the one the editor was filled for. A failure
    /// restores the shown note and shows even after navigation.
    private func saveNote(_ text: String) {
        guard let message = noteDraft else { return }
        let ticket = model.navigationTicket
        let sent = noteSent?.messageID == message.messageId ? noteSent?.text : nil
        let previous = sent ?? message.note ?? ""
        guard text != previous else { return }
        noteSent = (message.messageId, text)
        reads.run { [weak self] in
            guard let self else { return }
            do {
                try await self.model.setNote(text, messageID: message.messageId)
                guard ticket == self.model.navigationTicket else { return }
                await self.render()
            } catch {
                if self.noteSent?.messageID == message.messageId, self.noteSent?.text == text {
                    self.noteSent = (message.messageId, previous)
                }
                if self.noteDraft?.messageId == message.messageId { self.noteEditor.text = previous }
                self.showError(error)
            }
        }
    }

    private func rebuildAttachments() {
        for item in attachments.arrangedSubviews {
            attachments.removeArrangedSubview(item)
            item.removeFromSuperview()
        }
        for attachment in model.manifest?.attachments ?? [] {
            let size = attachment.bytes.map { ByteCountFormatter.string(fromByteCount: Int64(clamping: $0), countStyle: .file) } ?? "damaged"
            let button = NSButton(title: "\(attachment.displayName) (\(size))", target: self, action: #selector(openAttachment(_:)))
            button.identifier = NSUserInterfaceItemIdentifier(attachment.partID)
            button.setAccessibilityIdentifier("attachment-\(attachment.partID)")
            attachments.addArrangedSubview(button)
        }
        attachments.isHidden = attachments.arrangedSubviews.isEmpty
    }

    @objc private func openAttachment(_ sender: NSButton) {
        guard let partID = sender.identifier?.rawValue else { return }
        reads.run { await self.activateAttachment(partID) }
    }

    func activateAttachment(_ partID: String) async {
        let ticket = model.navigationTicket
        do {
            guard let url = try await model.openAttachment(partID: partID), ticket == model.navigationTicket else { return }
            if !opener(url) { showError(URLError(.cannotOpenFile)) }
        } catch {
            guard ticket == model.navigationTicket else { return }
            showError(error)
        }
    }

    /// Called only by a pane's current, user-activated link policy.
    func activateLink(_ url: URL, ticket: UInt64) async {
        // Both renditions share one click rule: the manifest lists the
        // current message's destinations and their warnings. Without one,
        // no Reading-view destination was recorded, so nothing admits.
        guard ticket == model.navigationTicket,
            model.manifest != nil || (model.showsReadingView && model.readingDocument != nil),
            let manifest = model.manifest ?? Self.emptyManifest
        else { return }
        switch ReaderLinkDecision.decide(url, manifest: manifest) {
        case .refuse: return
        case .open(let destination):
            if !opener(destination) { showError(URLError(.badURL)) }
        case .confirm(let destination, let reasons):
            guard await confirmer(destination, reasons), ticket == model.navigationTicket else { return }
            if !opener(destination) { showError(URLError(.badURL)) }
        }
    }

    private func showError(_ error: any Error) {
        status.stringValue = error.localizedDescription
        status.isHidden = false
    }

    /// A body lease without a manifest records no destinations; a
    /// Reading-view click is refused against it rather than guessed.
    private static let emptyManifest = try? RenditionManifest.parse(
        Data(#"{"v":1,"resources":[],"links":[],"attachments":[]}"#.utf8)
    )

    static func confirmLink(_ url: URL, reasons: [String]) -> Bool {
        let alert = NSAlert()
        alert.messageText = "Open this link?"
        alert.informativeText = ([url.absoluteString] + reasons).joined(separator: "\n\n")
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Open")
        alert.addButton(withTitle: "Cancel")
        return alert.runModal() == .alertFirstButtonReturn
    }
}

/// The default for controllers built without the app's shared owner:
/// senders cannot be remembered, so a write reports an error and the
/// toggle stays local to the open message.
private final class UnavailableReadingViewMemorySource: ReadingViewMemorySource {
    func readingViewSenders() async throws -> [String] {
        throw CoreFailure.unavailable(reason: "No Reading view sender list is installed.")
    }

    func setReadingViewSenders(_ senders: [String]) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "No Reading view sender list is installed.")
    }
}
