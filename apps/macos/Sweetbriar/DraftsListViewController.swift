// The Drafts (local) list (spec/08 "Drafts (local)"): one row per local
// draft over `DraftListModel`, in the core's order. Double-click or
// Return opens the row's draft through `onOpen`; a failed reload keeps
// the rows and shows the error with Retry. `ListPaneController` puts it
// in the main window's list pane in place of the message list.

import AppKit
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

    init(model: DraftListModel) {
        self.model = model
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError("DraftsListViewController is built in code") }

    override func loadView() {
        let column = NSTableColumn(identifier: Self.columnID)
        column.title = "Drafts"
        tableView.addTableColumn(column)
        tableView.headerView = nil
        tableView.rowHeight = 54
        tableView.style = .inset
        tableView.dataSource = self
        tableView.delegate = self
        tableView.target = self
        tableView.doubleAction = #selector(openChosen)
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

    @objc func openChosen() {
        let row = tableView.clickedRow >= 0 ? tableView.clickedRow : tableView.selectedRow
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
        cell.configure(model.rows[row])
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

/// One draft: subject, recipients, then "Queued" and the update time.
@MainActor
private final class DraftRowView: NSTableCellView {
    private let subject = NSTextField(labelWithString: "")
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
        let stack = NSStackView(views: [subject, recipients, detail])
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

    func configure(_ row: DraftListModel.Row) {
        subject.stringValue = row.subject
        recipients.stringValue = row.recipients
        recipients.isHidden = row.recipients.isEmpty
        let updated = row.updated.formatted(Self.updated)
        detail.stringValue = row.isQueued ? "Queued · \(updated)" : updated
        setAccessibilityIdentifier("draftRow-\(row.id)")
        setAccessibilityLabel(
            [row.subject, row.recipients, detail.stringValue].filter { !$0.isEmpty }
                .joined(separator: ", "))
    }
}

/// The main window's list pane: the message list, or the drafts list
/// in its place (spec/08 "Drafts (local)").
@MainActor
final class ListPaneController: NSViewController {
    let messages: NSViewController
    let drafts: DraftsListViewController
    private(set) var showsDrafts = false

    init(messages: NSViewController, drafts: DraftsListViewController) {
        self.messages = messages
        self.drafts = drafts
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) { fatalError("ListPaneController is built in code") }

    override func loadView() {
        let container = NSView()
        for child in [messages, drafts] {
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
        show(drafts: showsDrafts)
    }

    func show(drafts showDrafts: Bool) {
        showsDrafts = showDrafts
        guard isViewLoaded else { return }
        messages.view.isHidden = showDrafts
        drafts.view.isHidden = !showDrafts
    }
}
