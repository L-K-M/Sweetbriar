import AppKit
import SweetbriarBridge
import WebKit

@MainActor
final class ReaderViewController: NSViewController {
    let model: ReaderModel
    let header = NSTextField(wrappingLabelWithString: "")
    let accountChip = NSTextField(labelWithString: "")
    let status = NSTextField(wrappingLabelWithString: "No message selected")
    let remoteContent = NSTextField(labelWithString: "")
    let sourceButton = NSButton(title: "View Source", target: nil, action: nil)
    let attachments = NSStackView()
    let plainText = PlainTextReaderView()
    private let textScroll = NSScrollView()
    private let body = NSView()
    private let details = NSStackView()
    private let opener: @MainActor (URL) -> Bool
    private let confirmer: @MainActor (URL, [String]) async -> Bool
    private(set) var html: HTMLReaderView!

    init(
        model: ReaderModel,
        opener: @escaping @MainActor (URL) -> Bool = { NSWorkspace.shared.open($0) },
        confirmer: @escaping @MainActor (URL, [String]) async -> Bool = ReaderViewController.confirmLink,
        compileRules: @escaping @MainActor () async throws -> WKContentRuleList = { try await ReaderContentRules.compiled() }
    ) {
        self.model = model
        self.opener = opener
        self.confirmer = confirmer
        super.init(nibName: nil, bundle: nil)
        html = HTMLReaderView(currentTicket: { [weak model] in model?.navigationTicket ?? 0 }, compileRules: compileRules)
        html.linkActivated = { [weak self] url in
            guard let self else { return }
            let ticket = model.navigationTicket
            Task { await self.activateLink(url, ticket: ticket) }
        }
        html.ruleListUnavailable = { [weak self] ticket in
            Task { @MainActor [weak self] in
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
        sourceButton.target = self
        sourceButton.action = #selector(toggleSource)
        details.addArrangedSubview(sourceButton)
        remoteContent.textColor = .secondaryLabelColor
        remoteContent.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        details.addArrangedSubview(remoteContent)
        attachments.orientation = .vertical
        attachments.alignment = .leading
        details.addArrangedSubview(attachments)
        stack.addArrangedSubview(details)
        stack.addArrangedSubview(status)
        stack.addArrangedSubview(body)
        textScroll.hasVerticalScroller = true
        plainText.setAccessibilityIdentifier("readerBody")
        textScroll.documentView = plainText
        plainText.frame = NSRect(x: 0, y: 0, width: 400, height: 0)
        let panes: [NSView] = [textScroll, html!]
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
            stack.topAnchor.constraint(equalTo: view.topAnchor, constant: 12),
            stack.bottomAnchor.constraint(equalTo: view.bottomAnchor, constant: -12),
            body.widthAnchor.constraint(equalTo: stack.widthAnchor),
            header.widthAnchor.constraint(equalTo: stack.widthAnchor),
        ])
        body.setContentHuggingPriority(.defaultLow, for: .vertical)
        details.isHidden = true
        textScroll.isHidden = true
        html.isHidden = true
    }

    @discardableResult
    func select(_ selection: Set<Int64>) -> Task<Void, Never> {
        resetSelection(selection)
        let ticket = model.navigationTicket
        return Task {
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
        details.isHidden = true
        textScroll.isHidden = true
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
        await render()
    }

    @objc private func toggleSource() {
        Task { await setSource(!model.isShowingSource) }
    }

    func setSource(_ enabled: Bool) async {
        // Revoke WebKit's frame before requesting another rendition.
        html.isHidden = true
        await model.showSource(enabled)
        await render()
    }

    func close() {
        model.close()
        if isViewLoaded {
            html.clear(ticket: model.navigationTicket)
            plainText.text = ""
        }
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
            rebuildAttachments()
        }
        switch model.state {
        case .html(_, let rendition):
            html.isHidden = false
            await html.show(rendition, ticket: ticket)
        case .plainText(_, let text):
            plainText.mode = .text
            plainText.text = text
            textScroll.isHidden = false
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
            lines.append(Date(timeIntervalSince1970: Double(timestamp)).formatted(date: .abbreviated, time: .shortened))
        }
        if !message.tags.isEmpty { lines.append("Tags: " + message.tags.map(\.name).joined(separator: ", ")) }
        if let note = message.note, !note.isEmpty { lines.append("Note: " + note) }
        return lines.joined(separator: "\n")
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
        Task { await activateAttachment(partID) }
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

    /// Called only by the pane's current, user-activated navigation policy.
    func activateLink(_ url: URL, ticket: UInt64) async {
        guard ticket == model.navigationTicket, case .html = model.state, let manifest = model.manifest else { return }
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
