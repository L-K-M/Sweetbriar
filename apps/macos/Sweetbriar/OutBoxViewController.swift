// The Out Box pane (spec/08 "Out Box"): the live attempts, then a
// History group, each in the core's order. The selected row's details
// sit below the list, with the actions its state admits as buttons and
// as the row's context menu. `ListPaneController` puts it in the main
// window's list pane in place of the message list; the reader shows
// nothing while it is up.

import AppKit
import SwiftUI
import SweetbriarBridge

@MainActor
final class OutBoxViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate, NSMenuDelegate {
    let model: OutBoxModel
    let tableView = NSTableView()
    let errorLabel = NSTextField(wrappingLabelWithString: "")
    let retryButton = NSButton(title: "Retry", target: nil, action: nil)
    private let errorRow = NSStackView()
    private let emptyLabel = NSTextField(labelWithString: "The Out Box is empty")
    private let detailsStack = NSStackView()
    private let detailsScroll = NSScrollView()
    private let contextMenu = NSMenu()
    private var menuAttemptID: String?
    private var detailActions: [OutBoxModel.Action] = []
    private var reloads: [UUID: Task<Void, Never>] = [:]
    /// Actions in flight; a restore waits for them, as they write the
    /// archive it is about to swap.
    private var actions: [UUID: Task<Void, Never>] = [:]
    /// The attempt the user selected. The table's selected row can't
    /// stand in for it: by the time a reload renders, the rows it
    /// indexed have already changed.
    private var selectedID: String?
    private var polling: Task<Void, Never>?
    private var shown = false
    private static let columnID = NSUserInterfaceItemIdentifier("outBox")
    private static let cellID = NSUserInterfaceItemIdentifier("outBoxCell")
    private static let groupCellID = NSUserInterfaceItemIdentifier("outBoxGroupCell")

    /// The identities' colors for each row's chip; nil keeps them neutral.
    private let appearance: AppearanceModel?

    init(model: OutBoxModel, appearance: AppearanceModel? = nil) {
        self.model = model
        self.appearance = appearance
        super.init(nibName: nil, bundle: nil)
        if appearance != nil { observeAppearance() }
    }

    /// Recolor the rows' chips when an identity's appearance changes.
    private func observeAppearance() {
        withObservationTracking {
            _ = appearance?.appearance
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                if self.isViewLoaded { self.tableView.reloadData() }
                self.observeAppearance()
            }
        }
    }

    required init?(coder: NSCoder) { fatalError("OutBoxViewController is built in code") }

    deinit { polling?.cancel() }

    override func loadView() {
        let column = NSTableColumn(identifier: Self.columnID)
        column.title = "Out Box"
        tableView.addTableColumn(column)
        tableView.headerView = nil
        tableView.rowHeight = 74
        tableView.style = .inset
        tableView.dataSource = self
        tableView.delegate = self
        tableView.menu = contextMenu
        tableView.setAccessibilityIdentifier("outBoxList")
        let scroll = NSScrollView()
        scroll.documentView = tableView
        scroll.hasVerticalScroller = true

        contextMenu.delegate = self
        // Items follow the buttons: disabled while an action runs.
        contextMenu.autoenablesItems = false

        errorLabel.textColor = .secondaryLabelColor
        errorLabel.setAccessibilityIdentifier("outBoxError")
        retryButton.target = self
        retryButton.action = #selector(retry)
        retryButton.setAccessibilityIdentifier("outBoxRetry")
        errorRow.setViews([errorLabel, retryButton], in: .leading)
        errorRow.spacing = 8
        errorRow.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 0, right: 12)
        errorRow.isHidden = true

        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.alignment = .center
        emptyLabel.isHidden = true

        detailsStack.orientation = .vertical
        detailsStack.alignment = .leading
        detailsStack.spacing = 2
        detailsStack.edgeInsets = NSEdgeInsets(top: 6, left: 8, bottom: 6, right: 8)
        let detailsDocument = NSView()
        detailsDocument.translatesAutoresizingMaskIntoConstraints = false
        detailsStack.translatesAutoresizingMaskIntoConstraints = false
        detailsDocument.addSubview(detailsStack)
        detailsScroll.documentView = detailsDocument
        detailsScroll.hasVerticalScroller = true
        detailsScroll.autohidesScrollers = true
        detailsScroll.setAccessibilityIdentifier("outBoxDetails")
        // The details grow with their content, up to a bounded height.
        let fitsContent = detailsScroll.heightAnchor.constraint(equalTo: detailsDocument.heightAnchor)
        fitsContent.priority = .defaultLow
        NSLayoutConstraint.activate([
            detailsStack.leadingAnchor.constraint(equalTo: detailsDocument.leadingAnchor),
            detailsStack.trailingAnchor.constraint(equalTo: detailsDocument.trailingAnchor),
            detailsStack.topAnchor.constraint(equalTo: detailsDocument.topAnchor),
            detailsStack.bottomAnchor.constraint(equalTo: detailsDocument.bottomAnchor),
            detailsDocument.widthAnchor.constraint(equalTo: detailsScroll.contentView.widthAnchor),
            detailsScroll.heightAnchor.constraint(lessThanOrEqualToConstant: 180),
            fitsContent,
        ])

        let stack = NSStackView(views: [errorRow, scroll, detailsScroll])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        for view in [errorRow, scroll, detailsScroll] {
            view.translatesAutoresizingMaskIntoConstraints = false
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        // The table takes the pane's leftover height and gives it up
        // first; the details keep their own.
        scroll.setContentHuggingPriority(.defaultLow, for: .vertical)
        scroll.setContentCompressionResistancePriority(.defaultLow, for: .vertical)
        detailsScroll.setContentHuggingPriority(.defaultHigh, for: .vertical)
        detailsScroll.setContentCompressionResistancePriority(.defaultHigh, for: .vertical)
        emptyLabel.translatesAutoresizingMaskIntoConstraints = false
        stack.addSubview(emptyLabel)
        NSLayoutConstraint.activate([
            emptyLabel.centerXAnchor.constraint(equalTo: scroll.centerXAnchor),
            emptyLabel.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
        ])
        view = stack
    }

    override func viewDidLoad() {
        super.viewDidLoad()
        observe()
    }

    /// Render, and render again when the rows, errors or acting state
    /// change: an action disables its buttons while it runs.
    private func observe() {
        withObservationTracking {
            render()
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in self?.observe() }
        }
    }

    /// Re-read the list, then show it. Reloads coalesce in the model, so
    /// every caller renders after the latest read.
    func reload() {
        guard !model.isRetired else { return }
        let id = UUID()
        reloads[id] = Task { [weak self] in
            guard let self else { return }
            await model.reload()
            reloads[id] = nil
            render()
        }
    }

    /// The old archive is going away: polling stops, the rows go, and
    /// late reads are dropped.
    func retire() {
        polling?.cancel()
        polling = nil
        model.retire()
        render()
    }

    func waitForRetirement() async {
        for task in Array(actions.values) { await task.value }
        for task in Array(reloads.values) { await task.value }
    }

    /// Show or hide the pane. While it is shown with live rows, the list
    /// re-reads every 2 s so states move without input (spec/08).
    func setShown(_ shown: Bool) {
        guard shown != self.shown else { return }
        self.shown = shown
        guard shown else {
            polling?.cancel()
            polling = nil
            return
        }
        reload()
        startPolling()
    }

    private func startPolling() {
        guard polling == nil else { return }
        polling = Task { [weak self] in
            while !Task.isCancelled {
                try? await Task.sleep(for: .seconds(2))
                if Task.isCancelled { break }
                guard let self, self.shown, !self.model.isRetired else { break }
                if self.model.hasLiveRows { self.reload() }
            }
        }
    }

    func render() {
        guard isViewLoaded else { return }
        // The selection is held by attempt id, so a reload keeps it even
        // when rows are added or removed above it.
        let selected = selectedID
        tableView.reloadData()
        if let id = selected, let index = model.rows.firstIndex(where: { $0.id == id }) {
            tableView.selectRowIndexes(
                IndexSet(integer: displayRow(forModelIndex: index)), byExtendingSelection: false)
        } else {
            tableView.deselectAll(nil)
        }
        if let error = model.lastError {
            errorLabel.stringValue = "Couldn't load the Out Box: \(error.localizedDescription)"
            errorRow.isHidden = false
        } else {
            errorRow.isHidden = true
        }
        emptyLabel.isHidden = !model.rows.isEmpty || model.lastError != nil || model.isRetired
        renderDetails(selected)
    }

    /// The selected row's recipients, Proton's reply and evidence, the
    /// Sent-copy warning, the last action's failure, and the actions its
    /// state admits.
    private func renderDetails(_ id: String?) {
        for view in detailsStack.arrangedSubviews {
            detailsStack.removeArrangedSubview(view)
            view.removeFromSuperview()
        }
        guard let id, let row = model.rows.first(where: { $0.id == id }) else { return }
        let attempt = row.row
        for recipient in attempt.recipients {
            addDetail(Self.recipientLine(recipient))
        }
        if let reply = attempt.finalReply, !reply.isEmpty {
            addDetail("Proton's reply: \(reply)")
        }
        for line in attempt.evidence { addDetail(line) }
        if attempt.protonDraftId != nil,
           attempt.state == .failed
            || (attempt.state == .outcomeUnknown && attempt.resolution != nil)
        {
            addDetail("Left in Proton's Drafts")
        }
        if attempt.sentCopyWarning {
            addDetail("The Sent copy hasn't appeared yet.")
        }
        if let error = model.actionError {
            addDetail(error, color: .systemRed)
        }
        detailActions = model.actions(for: id)
        for (index, action) in detailActions.enumerated() {
            let button = NSButton(
                title: Self.label(for: action).title, target: self,
                action: #selector(actionClicked(_:)))
            button.tag = index
            button.isEnabled = !model.isActing
            button.setAccessibilityIdentifier(
                "outBoxAction-\(Self.label(for: action).name)")
            detailsStack.addArrangedSubview(button)
        }
    }

    private func addDetail(_ text: String, color: NSColor = .labelColor) {
        let label = NSTextField(wrappingLabelWithString: text)
        label.textColor = color
        label.lineBreakMode = .byWordWrapping
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        detailsStack.addArrangedSubview(label)
    }

    @objc private func retry() { reload() }

    /// Run `action` on `id`, asking first when it can deliver a message
    /// twice; the model refuses anything the row doesn't admit.
    private func run(_ action: OutBoxModel.Action, on id: String) {
        guard !model.isActing, model.actions(for: id).contains(action) else { return }
        guard OutBoxModel.needsConfirmation(action) else {
            perform(action, on: id)
            return
        }
        confirm(action) { [weak self] confirmed in
            guard confirmed else { return }
            self?.perform(action, on: id)
        }
    }

    func perform(_ action: OutBoxModel.Action, on id: String) {
        let task = UUID()
        actions[task] = Task { [weak self] in
            guard let self else { return }
            await model.perform(action, on: id)
            actions[task] = nil
            render()
        }
    }

    /// The duplicate warnings (spec/08 "Out Box"): Send Again and Keep
    /// as Draft ask first, as a sheet on the pane's window.
    private func confirm(
        _ action: OutBoxModel.Action, then confirm: @escaping @MainActor (Bool) -> Void
    ) {
        let alert = NSAlert()
        switch action {
        case .sendAgain:
            alert.messageText = "Send this message again?"
            alert.informativeText =
                "Recipients may get it twice: the first send may have arrived. "
                    + "The copy keeps its Message-ID, so their mail apps can spot the duplicate."
            alert.addButton(withTitle: "Send Again")
            alert.addButton(withTitle: "Cancel")
        case .keepAsDraft:
            alert.messageText = "Keep this message as a draft?"
            alert.informativeText =
                "If the first send arrived, sending the same draft again may deliver it twice."
            alert.addButton(withTitle: "Keep as Draft")
            alert.addButton(withTitle: "Cancel")
        default:
            // Fail safe: an action marked as asking without a dialog
            // here is a programming error, never a silent yes.
            assertionFailure("\(action) needs a confirmation dialog")
            confirm(false)
            return
        }
        guard let window = view.window else {
            confirm(alert.runModal() == .alertFirstButtonReturn)
            return
        }
        alert.beginSheetModal(for: window) { confirm($0 == .alertFirstButtonReturn) }
    }

    @objc private func actionClicked(_ sender: NSButton) {
        guard sender.tag >= 0, sender.tag < detailActions.count,
              let id = selectedID
        else { return }
        run(detailActions[sender.tag], on: id)
    }

    @objc private func menuAction(_ sender: NSMenuItem) {
        guard let action = sender.representedObject as? OutBoxModel.Action,
              let id = menuAttemptID
        else { return }
        run(action, on: id)
    }

    /// The History group sits before the first history row; it isn't a
    /// row and offers nothing.
    private var groupRow: Int? { model.rows.firstIndex(where: { $0.isHistory }) }

    /// The model's index for a table row, or nil for the group row and
    /// rows that aren't listed.
    private func modelIndex(forTableRow row: Int) -> Int? {
        guard row >= 0 else { return nil }
        if let group = groupRow {
            if row == group { return nil }
            let index = row > group ? row - 1 : row
            return model.rows.indices.contains(index) ? index : nil
        }
        return model.rows.indices.contains(row) ? row : nil
    }

    private func displayRow(forModelIndex index: Int) -> Int {
        if let group = groupRow, index >= group { return index + 1 }
        return index
    }

    private func selectedAttemptID() -> String? {
        guard let index = modelIndex(forTableRow: tableView.selectedRow) else { return nil }
        return model.rows[index].id
    }

    func numberOfRows(in tableView: NSTableView) -> Int {
        model.rows.count + (groupRow == nil ? 0 : 1)
    }

    func tableView(_ tableView: NSTableView, isGroupRow row: Int) -> Bool {
        row == groupRow
    }

    func tableView(_ tableView: NSTableView, heightOfRow row: Int) -> CGFloat {
        row == groupRow ? 24 : 54
    }

    func tableView(_ tableView: NSTableView, shouldSelectRow row: Int) -> Bool {
        row != groupRow
    }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        if row == groupRow {
            let cell =
                tableView.makeView(withIdentifier: Self.groupCellID, owner: nil) as? GroupRowView
                ?? GroupRowView(identifier: Self.groupCellID)
            cell.label.stringValue = "History"
            return cell
        }
        guard let index = modelIndex(forTableRow: row) else { return nil }
        let cell =
            tableView.makeView(withIdentifier: Self.cellID, owner: nil) as? OutBoxRowView
            ?? OutBoxRowView(identifier: Self.cellID)
        cell.configure(model.rows[index], appearance: appearance?.appearance)
        return cell
    }

    func tableViewSelectionDidChange(_ notification: Notification) {
        selectedID = selectedAttemptID()
        renderDetails(selectedID)
    }

    /// The clicked row's context menu offers exactly its actions.
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        menuAttemptID = nil
        guard let index = modelIndex(forTableRow: tableView.clickedRow) else { return }
        let id = model.rows[index].id
        menuAttemptID = id
        for action in model.actions(for: id) {
            let item = NSMenuItem(
                title: Self.label(for: action).title, action: #selector(menuAction(_:)),
                keyEquivalent: "")
            item.target = self
            item.identifier = MainMenuBuilder.ItemID.command("outBox.\(Self.label(for: action).name)")
            item.isEnabled = !model.isActing
            item.representedObject = action
            menu.addItem(item)
        }
    }

    /// The button title and the accessibility identifier's case name.
    private static func label(for action: OutBoxModel.Action) -> (title: String, name: String) {
        switch action {
        case .cancelSend: return ("Cancel Send", "cancelSend")
        case .confirmAndSend: return ("Confirm and Send", "confirmAndSend")
        case .markAsSent: return ("Mark as Sent", "markAsSent")
        case .sendAgain: return ("Send Again…", "sendAgain")
        case .keepAsDraft: return ("Keep as Draft…", "keepAsDraft")
        case .retryRejectedRecipients:
            return ("Retry Rejected Recipients", "retryRejectedRecipients")
        }
    }

    /// One recipient's detail line: its field, address and state, and
    /// "Not end-to-end encrypted" for one Proton was asked to send in
    /// clear (spec/08 "Out Box", spec/05 step 1).
    static func recipientLine(_ recipient: OutBoxRecipient) -> String {
        let line =
            "\(Self.field(recipient.kind)): \(recipient.address) — "
            + Self.stateWords(recipient.state)
        guard recipient.scheme == .clear else { return line }
        return line + " · Not end-to-end encrypted"
    }

    private static func field(_ kind: RecipientKind) -> String {
        switch kind {
        case .to: return "To"
        case .cc: return "Cc"
        case .bcc: return "Bcc"
        }
    }

    private static func stateWords(_ state: RecipientState) -> String {
        switch state {
        case .pending: return "Pending"
        case .rcptAccepted: return "Accepted by server"
        case .rejectedPermanent: return "Rejected"
        case .rejectedTransient: return "Rejected (temporary)"
        case .sent: return "Sent"
        case .unsent: return "Not sent"
        case .unknown: return "Unknown"
        }
    }
}

/// One Out Box row: the frozen subject, the To then Cc addresses, then
/// the state and the update time.
@MainActor
final class OutBoxRowView: NSTableCellView {
    private let subject = NSTextField(labelWithString: "")
    /// The route the attempt froze, apart from its state (spec/08 "Out Box").
    let from = NSHostingView(rootView: AccountChip(label: "", accountId: "", appearance: nil))
    private let recipients = NSTextField(labelWithString: "")
    private let detail = NSTextField(labelWithString: "")
    private static let updated = Date.FormatStyle(date: .abbreviated, time: .shortened)

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        subject.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
        for label in [subject, recipients, detail] {
            label.lineBreakMode = .byTruncatingTail
            label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        }
        recipients.textColor = .secondaryLabelColor
        detail.textColor = .secondaryLabelColor
        detail.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        from.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        let stack = NSStackView(views: [subject, from, recipients, detail])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 1
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            stack.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -4),
            stack.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("OutBoxRowView is built in code") }

    func configure(_ row: OutBoxModel.Row, appearance: Appearance?) {
        subject.stringValue = row.subject
        from.rootView = AccountChip(
            label: row.from, owner: .identity(id: row.row.identityId, accountId: row.row.accountId),
            appearance: appearance, identifier: "outBoxFrom-\(row.id)")
        recipients.stringValue = row.recipients
        recipients.isHidden = row.recipients.isEmpty
        detail.stringValue = "\(row.status) · \(row.updated.formatted(Self.updated))"
        setAccessibilityLabel(
            [row.subject, "From \(row.from)", row.status, row.recipients].filter { !$0.isEmpty }
                .joined(separator: ", "))
    }
}

/// The History group header, above the first history row.
@MainActor
private final class GroupRowView: NSTableCellView {
    let label = NSTextField(labelWithString: "")

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        label.font = .boldSystemFont(ofSize: NSFont.smallSystemFontSize)
        label.textColor = .secondaryLabelColor
        label.lineBreakMode = .byTruncatingTail
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        label.translatesAutoresizingMaskIntoConstraints = false
        addSubview(label)
        NSLayoutConstraint.activate([
            label.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 4),
            label.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor, constant: -4),
            label.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("GroupRowView is built in code") }
}
