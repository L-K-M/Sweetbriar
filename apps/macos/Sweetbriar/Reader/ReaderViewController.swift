import AppKit
import SwiftUI
import SweetbriarBridge
import WebKit

/// Which way Space pages the reader (spec/08 "Message window").
enum ReaderPage: Equatable {
    case down
    case up
}

@MainActor
final class ReaderViewController: NSViewController {
    let model: ReaderModel
    let header = NSTextField(wrappingLabelWithString: "")
    /// "Received in <account>" (spec/08 "Account chips").
    let accountChip = NSHostingView(rootView: AccountChip(label: "", accountId: "", appearance: nil))
    private let appearance: AppearanceModel?
    let tagPicker = TagPickerView(frame: .zero)
    let noteEditor = NoteEditorView(frame: .zero)
    /// The note last sent: the shown note may lag a write in flight.
    private var noteSent: (messageID: Int64, text: String)?
    /// The message the note editor was filled for, and its note then.
    private var noteDraft: (messageId: Int64, note: String?)?
    /// Loading and error notes, above the body.
    let status = NSTextField(wrappingLabelWithString: "")
    /// The pane without a message, centered as Mail's: none or several
    /// selected.
    let placeholder = NSTextField(wrappingLabelWithString: "No message selected")
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
    /// Draws a message window's header tint (spec/08 Message window
    /// "Tint"); neutral, it draws nothing.
    let backdrop = ReaderBackdropView()
    /// The tint the header shows, if any.
    private(set) var headerTint: MessageWindowTint?
    private let reads = ArchiveViewTasks()
    private let memory: ReadingViewMemory
    private let opener: @MainActor (URL) -> Bool
    private let confirmer: @MainActor (URL, [String]) async -> Bool
    /// spec/08's `open_message` span, from a single selection to its body.
    private let latency: LatencyRecorder
    private(set) var html: HTMLReaderView!

    init(
        model: ReaderModel,
        opener: @escaping @MainActor (URL) -> Bool = { NSWorkspace.shared.open($0) },
        confirmer: @escaping @MainActor (URL, [String]) async -> Bool = ReaderViewController.confirmLink,
        readResource: @escaping @Sendable (URL) async throws -> Data = { try Data(contentsOf: $0) },
        compileRules: @escaping @MainActor () async throws -> WKContentRuleList = { try await ReaderContentRules.compiled() },
        memory: ReadingViewMemory = ReadingViewMemory(
            source: UnavailableReadingViewMemorySource()
        ),
        appearance: AppearanceModel? = nil,
        latency: LatencyRecorder = .shared
    ) {
        self.model = model
        self.appearance = appearance
        self.latency = latency
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
        html.didShowBody = { [weak self] ticket in
            guard let self, ticket == model.navigationTicket, let id = model.occurrenceID else { return }
            latency.end(.openMessage, key: id, shownIn: html)
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
        if appearance != nil { observeAppearance() }
    }

    required init?(coder: NSCoder) { fatalError("ReaderViewController is built in code") }

    /// Recolor the chip when an account's appearance changes.
    private func observeAppearance() {
        withObservationTracking {
            _ = appearance?.appearance
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                if let message = self.model.currentMessage { self.renderAccountChip(message) }
                self.observeAppearance()
            }
        }
    }

    private func renderAccountChip(_ message: ReaderMessage) {
        accountChip.rootView = AccountChip(
            label: "Received in \(message.accountDisplayName)", accountId: message.accountId,
            appearance: appearance?.appearance, identifier: "readerAccount")
    }

    override func loadView() {
        view = backdrop
        backdrop.headerEnd = details
        let stack = NSStackView()
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 8
        stack.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(stack)
        details.orientation = .vertical
        details.alignment = .leading
        details.spacing = 6
        header.isSelectable = true
        // Keeps the header's fonts and colors while text is selected.
        header.allowsEditingTextAttributes = true
        details.addArrangedSubview(header)
        details.addArrangedSubview(accountChip)
        details.addArrangedSubview(tagPicker)
        details.addArrangedSubview(noteEditor)
        sourceButton.target = self
        sourceButton.action = #selector(toggleSource)
        readingViewToggle.setAccessibilityIdentifier("readingViewToggle")
        readingViewToggle.target = self
        readingViewToggle.action = #selector(toggleReadingView(_:))
        // The renditions side by side, each one click away (spec/08).
        let renditions = NSStackView(views: [sourceButton, readingViewToggle])
        renditions.orientation = .horizontal
        renditions.spacing = 12
        details.addArrangedSubview(renditions)
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
        placeholder.font = .preferredFont(forTextStyle: .title2)
        placeholder.textColor = .secondaryLabelColor
        placeholder.alignment = .center
        placeholder.isSelectable = false
        placeholder.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(placeholder)
        NSLayoutConstraint.activate([
            placeholder.centerXAnchor.constraint(equalTo: view.centerXAnchor),
            placeholder.centerYAnchor.constraint(equalTo: view.safeAreaLayoutGuide.centerYAnchor),
            placeholder.leadingAnchor.constraint(greaterThanOrEqualTo: view.leadingAnchor, constant: 20),
            placeholder.trailingAnchor.constraint(lessThanOrEqualTo: view.trailingAnchor, constant: -20),
        ])
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
            noteEditor.heightAnchor.constraint(equalToConstant: 40),
        ])
        body.setContentHuggingPriority(.defaultLow, for: .vertical)
        details.isHidden = true
        status.isHidden = true
        textScroll.isHidden = true
        readingScroll.isHidden = true
        html.isHidden = true
        tagPicker.onAdd = { [weak self] name in self?.setTag(name, applied: true) }
        tagPicker.onRemove = { [weak self] tag in self?.setTag(tag.name, applied: false) }
        noteEditor.onEndEditing = { [weak self] text in self?.saveNote(text) }
    }

    /// Tint the header area for a message window, or return it to
    /// neutral: the backdrop draws the fill and the header's labels take
    /// its foreground (spec/08 Message window "Tint").
    func applyHeaderTint(_ tint: MessageWindowTint?) {
        loadViewIfNeeded()
        guard tint != headerTint else { return }
        headerTint = tint
        backdrop.tint = tint
        header.textColor = tint?.foreground ?? .labelColor
        if let message = model.currentMessage { renderHeader(message) }
        for label in [remoteContent, readingNote] {
            label.textColor = tint?.foreground ?? .secondaryLabelColor
        }
        let title = readingViewToggle.title
        if let tint {
            readingViewToggle.attributedTitle = NSAttributedString(
                string: title,
                attributes: [.foregroundColor: tint.foreground, .font: readingViewToggle.font ?? .systemFont(ofSize: 0)])
        } else {
            readingViewToggle.title = title
        }
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
        placeholder.isHidden = true
        guard selection.count == 1, let id = selection.first else {
            latency.cancel(.openMessage)
            status.isHidden = true
            placeholder.stringValue = selection.isEmpty ? "No message selected" : "\(selection.count) messages selected"
            placeholder.isHidden = false
            return
        }
        latency.begin(.openMessage, replacing: id)
        status.stringValue = "Loading message…"
    }

    private func loadSelection(_ selection: Set<Int64>) async {
        guard selection.count == 1, let id = selection.first else { return }
        await model.show(occurrenceID: id)
        updateReadingToggle()
        clearReadingNote()
        await render()
    }

    /// Scroll the shown body a page, as Space does in the list
    /// (spec/08 "Message window"); nothing when no body is shown.
    func page(_ direction: ReaderPage) {
        if !html.isHidden {
            switch direction {
            case .down: html.webView.scrollPageDown(nil)
            case .up: html.webView.scrollPageUp(nil)
            }
            return
        }
        guard let scroll = [textScroll, readingScroll].first(where: { !$0.isHidden }) else { return }
        Self.page(scroll, direction)
    }

    /// Move `scroll` a visible height less AppKit's page overlap,
    /// within its document, by moving its clip view.
    static func page(_ scroll: NSScrollView, _ direction: ReaderPage) {
        let clip = scroll.contentView
        let visible = clip.bounds
        let step = max(visible.height - scroll.verticalPageScroll, visible.height / 2)
        let bottom = max((scroll.documentView?.frame.height ?? 0) - visible.height, 0)
        let wanted = visible.origin.y + (direction == .down ? step : -step)
        clip.scroll(to: NSPoint(x: visible.origin.x, y: min(max(wanted, 0), bottom)))
        scroll.reflectScrolledClipView(clip)
    }

    /// The subject of the message shown, for a message window's title.
    var shownSubject: String? {
        switch model.state {
        case .plainText(let message, _), .source(let message, _), .html(let message, _):
            message.headers.subject
        default:
            nil
        }
    }

    /// "1 remote content item blocked", "3 remote content items blocked".
    private static func remoteContentText(_ blocked: UInt32) -> String {
        "\(blocked) remote content \(blocked == 1 ? "item" : "items") blocked"
    }

    /// View → Message Source answers while a message is shown.
    var canToggleSource: Bool { model.currentMessage != nil }
    var isShowingSource: Bool { model.isShowingSource }
    /// View → Reading View answers when the message has a reading
    /// document, as the toggle does.
    var canToggleReadingView: Bool { model.readingDocument != nil }
    var isShowingReadingView: Bool { model.showsReadingView }

    /// View → Message Source: the View Source button's switch.
    func toggleSourceFromMenu() {
        guard canToggleSource else { return }
        toggleSource()
    }

    /// View → Reading View: the toggle's switch, for this message and,
    /// as the toggle does, its sender.
    func toggleReadingViewFromMenu() {
        guard canToggleReadingView else { return }
        let enabled = !model.showsReadingView
        reads.run { await self.setReadingView(enabled) }
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
            renderHeader(message)
            renderAccountChip(message)
            sourceButton.title = model.isShowingSource ? "Sender Layout" : "View Source"
            let blocked = model.currentLease?.remoteContentBlocked ?? 0
            remoteContent.stringValue = Self.remoteContentText(blocked)
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
        noteOpen(showingReading: showReading)
    }

    /// spec/08 "Latency log": the body a selection opened is on screen,
    /// or this open won't show one. An open that waits for Proton to
    /// fetch the body isn't a cached open, so it isn't recorded.
    private func noteOpen(showingReading: Bool) {
        guard let id = model.occurrenceID else { return }
        // `open_body` fetches a body that wasn't on this Mac (spec/07):
        // this open waited for Proton.
        if let message = shownMessage, message.bodyAvailability != .acquired {
            latency.cancel(.openMessage, key: id)
            return
        }
        switch model.state {
        case .html where !showingReading:
            // WebKit shows it when the navigation finishes (`didShowBody`).
            break
        case .html:
            latency.end(.openMessage, key: id, shownIn: readingScroll)
        case .plainText:
            latency.end(.openMessage, key: id, shownIn: showingReading ? readingScroll : textScroll)
        case .loading, .empty:
            break
        case .source, .unavailable, .failed:
            latency.cancel(.openMessage, key: id)
        }
    }

    /// The message whose body or source is shown, as it was before the
    /// open.
    private var shownMessage: ReaderMessage? {
        switch model.state {
        case .html(let message, _), .plainText(let message, _), .source(let message, _): message
        case .empty, .loading, .unavailable, .failed: nil
        }
    }

    private func renderHeader(_ message: ReaderMessage) {
        header.attributedStringValue = Self.headerText(
            message, primary: headerTint?.foreground ?? .labelColor,
            secondary: headerTint?.foreground.withAlphaComponent(0.75) ?? .secondaryLabelColor)
    }

    /// The header as Mail lays it out: the subject in bold, then From
    /// with the sender's name in bold, To, Cc when there is one, and the
    /// date, their labels and addresses muted. Addresses stay visible:
    /// a name alone could pass for anyone.
    static func headerText(
        _ message: ReaderMessage, primary: NSColor = .labelColor,
        secondary: NSColor = .secondaryLabelColor
    ) -> NSAttributedString {
        let font = NSFont.systemFont(ofSize: NSFont.systemFontSize)
        let bold = NSFont.boldSystemFont(ofSize: NSFont.systemFontSize)
        let title = NSFont.boldSystemFont(ofSize: NSFont.preferredFont(forTextStyle: .title3).pointSize)
        let text = NSMutableAttributedString()
        func append(_ string: String, _ font: NSFont, _ color: NSColor) {
            text.append(NSAttributedString(string: string, attributes: [.font: font, .foregroundColor: color]))
        }
        func line(_ label: String, _ values: [ReaderAddress], names: NSFont) {
            guard !values.isEmpty else { return }
            append("\n\(label) ", font, secondary)
            for (index, value) in values.enumerated() {
                if index > 0 { append(", ", font, primary) }
                switch (value.name, value.address) {
                case (let name?, let address?):
                    append(name, names, primary)
                    append(" <\(address)>", font, secondary)
                case (let name?, nil): append(name, names, primary)
                case (nil, let address?): append(address, names, primary)
                case (nil, nil): append("Unknown", names, primary)
                }
            }
        }
        let subject = message.headers.subject?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        append(subject.isEmpty ? "(No Subject)" : subject, title, primary)
        line("From:", message.headers.from, names: bold)
        line("To:", message.headers.to, names: font)
        line("Cc:", message.headers.cc, names: font)
        if let timestamp = message.headers.date {
            let seconds = Duration.milliseconds(timestamp) / .seconds(1)
            let date = Date(timeIntervalSince1970: seconds).formatted(date: .abbreviated, time: .shortened)
            append("\n\(date)", font, secondary)
        }
        let paragraph = NSMutableParagraphStyle()
        paragraph.paragraphSpacing = 2
        text.addAttribute(.paragraphStyle, value: paragraph, range: NSRange(location: 0, length: text.length))
        return text
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
