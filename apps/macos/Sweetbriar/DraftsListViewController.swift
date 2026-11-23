// The Drafts (local) list (spec/08 "Drafts (local)"): one row per local
// draft over `DraftListModel`, in the core's order. Double-click or
// Return opens the row's draft through `onOpen`; a failed reload keeps
// the rows and shows the error with Retry. `ListPaneController` puts it,
// or the Out Box, in the main window's list pane in place of the
// message list.

import AppKit
import SwiftUI
import SweetbriarBridge

@MainActor
final class DraftsListViewController: NSViewController, NSTableViewDataSource, NSTableViewDelegate {
    let model: DraftListModel
    /// Open the draft with this id.
    var onOpen: (@MainActor (String) -> Void)?
    let tableView = DraftsTableView()
    let errorLabel = NSTextField(wrappingLabelWithString: "")
    let retryButton = NSButton(title: "Retry", target: nil, action: nil)
    private let errorRow = NSStackView()
    private let emptyLabel = NSTextField(labelWithString: "No local drafts")
    private var reloads: [UUID: Task<Void, Never>] = [:]
    private static let columnID = NSUserInterfaceItemIdentifier("draft")
    private static let cellID = NSUserInterfaceItemIdentifier("draftCell")

    /// The identities' colors for each row's chip; nil keeps them neutral.
    private let appearance: AppearanceModel?

    init(model: DraftListModel, appearance: AppearanceModel? = nil) {
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

    required init?(coder: NSCoder) { fatalError("DraftsListViewController is built in code") }

    override func loadView() {
        let column = NSTableColumn(identifier: Self.columnID)
        column.title = "Drafts"
        tableView.addTableColumn(column)
        tableView.headerView = nil
        tableView.rowHeight = 74
        tableView.style = .inset
        tableView.dataSource = self
        tableView.delegate = self
        tableView.target = self
        tableView.doubleAction = #selector(openClicked)
        tableView.onReturn = { [weak self] in self?.openChosen() }
        tableView.setAccessibilityIdentifier("draftsList")
        let scroll = NSScrollView()
        scroll.documentView = tableView
        scroll.hasVerticalScroller = true

        errorLabel.textColor = .secondaryLabelColor
        errorLabel.setAccessibilityIdentifier("draftsError")
        retryButton.target = self
        retryButton.action = #selector(retry)
        retryButton.setAccessibilityIdentifier("draftsRetry")
        errorRow.setViews([errorLabel, retryButton], in: .leading)
        errorRow.spacing = 8
        errorRow.edgeInsets = NSEdgeInsets(top: 8, left: 12, bottom: 0, right: 12)
        errorRow.isHidden = true

        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.alignment = .center
        emptyLabel.isHidden = true

        let stack = NSStackView(views: [errorRow, scroll])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        for view in [errorRow, scroll] {
            view.translatesAutoresizingMaskIntoConstraints = false
            view.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true
        }
        emptyLabel.translatesAutoresizingMaskIntoConstraints = false
        stack.addSubview(emptyLabel)
        NSLayoutConstraint.activate([
            emptyLabel.centerXAnchor.constraint(equalTo: scroll.centerXAnchor),
            emptyLabel.centerYAnchor.constraint(equalTo: scroll.centerYAnchor),
        ])
        view = stack
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

    /// The old archive is going away: nothing opens and late reads are
    /// dropped.
    func retire() {
        onOpen = nil
        model.retire()
        render()
    }

    func waitForRetirement() async {
        for task in Array(reloads.values) { await task.value }
    }

    func render() {
        guard isViewLoaded else { return }
        tableView.reloadData()
        if let error = model.lastError {
            errorLabel.stringValue = "Couldn't load drafts: \(error.localizedDescription)"
            errorRow.isHidden = false
        } else {
            errorRow.isHidden = true
        }
        emptyLabel.isHidden = !model.rows.isEmpty || model.lastError != nil || model.isRetired
    }

    @objc private func retry() { reload() }

    /// Return and the tests open the selection; only a double-click
    /// opens the clicked row, so Return never reopens an earlier click.
    func openChosen() { open(row: tableView.selectedRow) }

    @objc private func openClicked() {
        open(row: tableView.clickedRow >= 0 ? tableView.clickedRow : tableView.selectedRow)
    }

    private func open(row: Int) {
        guard model.rows.indices.contains(row), let id = model.draftId(forRow: model.rows[row].id) else {
            return
        }
        onOpen?(id)
    }

    func numberOfRows(in tableView: NSTableView) -> Int { model.rows.count }

    func tableView(_ tableView: NSTableView, viewFor tableColumn: NSTableColumn?, row: Int) -> NSView? {
        guard model.rows.indices.contains(row) else { return nil }
        let cell =
            tableView.makeView(withIdentifier: Self.cellID, owner: nil) as? DraftRowView
            ?? DraftRowView(identifier: Self.cellID)
        cell.configure(model.rows[row], appearance: appearance?.appearance)
        return cell
    }
}

/// A table that opens its selection on Return.
@MainActor
final class DraftsTableView: NSTableView {
    var onReturn: (@MainActor () -> Void)?

    override func keyDown(with event: NSEvent) {
        // Return, or Enter on the keypad.
        if ["\r", "\u{3}"].contains(event.charactersIgnoringModifiers), let onReturn {
            onReturn()
            return
        }
        super.keyDown(with: event)
    }
}

/// One draft: subject, its From identity, recipients, then "Queued" and
/// the update time.
@MainActor
final class DraftRowView: NSTableCellView {
    private let subject = NSTextField(labelWithString: "")
    /// The From identity's chip (spec/08 "Drafts (local)").
    private let from = NSHostingView(rootView: AccountChip(label: "", accountId: "", appearance: nil))
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

    required init?(coder: NSCoder) { fatalError("DraftRowView is built in code") }

    func configure(_ row: DraftListModel.Row, appearance: Appearance?) {
        subject.stringValue = row.subject
        let (label, owner) = Self.from(row.from)
        from.rootView = AccountChip(
            label: label, owner: owner ?? .account(""), appearance: owner == nil ? nil : appearance,
            identifier: "draftFrom-\(row.id)")
        recipients.stringValue = row.recipients
        recipients.isHidden = row.recipients.isEmpty
        let updated = row.updated.formatted(Self.updated)
        detail.stringValue = row.isQueued ? "Queued · \(updated)" : updated
        setAccessibilityIdentifier("draftRow-\(row.id)")
        setAccessibilityLabel(
            [row.subject, Self.spokenFrom(row.from), row.recipients, detail.stringValue]
                .filter { !$0.isEmpty }
                .joined(separator: ", "))
    }

    /// What VoiceOver reads for the From line: "From" and the identity,
    /// or the row's own words when there is none (spec/08 "Drafts
    /// (local)"), never "From No sending identity".
    static func spokenFrom(_ from: DraftListModel.From) -> String {
        let (label, owner) = Self.from(from)
        return owner == nil ? label : "From \(label)"
    }

    /// A row's From label, and the identity whose chip it is: none when
    /// the draft has no identity or its identity is unavailable.
    static func from(_ from: DraftListModel.From) -> (label: String, owner: AccountChip.Owner?) {
        switch from {
        case .none:
            ("No sending identity", nil)
        case .unavailable:
            ("Sending identity unavailable", nil)
        case .identity(let identity):
            (
                IdentityStripContent.label(for: identity),
                .identity(id: identity.identityId, accountId: identity.accountId)
            )
        }
    }
}

/// The main window's list pane: the message list, or the drafts list,
/// or the Out Box, in its place (spec/08).
@MainActor
final class ListPaneController: NSViewController {
    enum Pane {
        case messages
        case drafts
        case outBox
    }

    let messages: NSViewController
    let drafts: DraftsListViewController
    let outBox: OutBoxViewController
    private(set) var pane: Pane = .messages

    init(messages: NSViewController, drafts: DraftsListViewController, outBox: OutBoxViewController) {
        self.messages = messages
        self.drafts = drafts
        self.outBox = outBox
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError("ListPaneController is built in code") }

    override func loadView() {
        let container = NSView()
        for child in [messages, drafts, outBox] {
            addChild(child)
            child.view.translatesAutoresizingMaskIntoConstraints = false
            container.addSubview(child.view)
            NSLayoutConstraint.activate([
                child.view.topAnchor.constraint(equalTo: container.topAnchor),
                child.view.bottomAnchor.constraint(equalTo: container.bottomAnchor),
                child.view.leadingAnchor.constraint(equalTo: container.leadingAnchor),
                child.view.trailingAnchor.constraint(equalTo: container.trailingAnchor),
            ])
        }
        view = container
        show(pane)
    }

    func show(_ pane: Pane) {
        self.pane = pane
        guard isViewLoaded else { return }
        messages.view.isHidden = pane != .messages
        drafts.view.isHidden = pane != .drafts
        outBox.view.isHidden = pane != .outBox
        outBox.setShown(pane == .outBox)
    }
}
