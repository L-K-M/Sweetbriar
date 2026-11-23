// The filing palette (spec/08 "Keyboard filing"): a panel sheet with a
// search field over one account's mailboxes. The field keeps the
// keyboard: Up and Down choose, Return files, Escape changes nothing.

import AppKit
import SweetbriarBridge

/// What the palette chooses a mailbox for.
enum FilingChoice {
    case move
    case addLabel
    case removeLabel

    var title: String {
        switch self {
        case .move: "Move to…"
        case .addLabel: "Add Label…"
        case .removeLabel: "Remove Label…"
        }
    }

    var verb: String {
        switch self {
        case .move: "Move"
        case .addLabel: "Add"
        case .removeLabel: "Remove"
        }
    }

    func action(_ mailbox: Mailbox) -> FilingAction {
        switch self {
        case .move: .move(to: mailbox)
        case .addLabel: .addLabel(mailbox)
        case .removeLabel: .removeLabel(mailbox)
        }
    }

    /// The account's mailboxes this choice can be made from.
    func candidates(in mailboxes: [Mailbox], shownMailboxId: Int64?) -> [Mailbox] {
        switch self {
        case .move: FilingDestinations.folders(in: mailboxes, shownMailboxId: shownMailboxId)
        case .addLabel, .removeLabel: FilingDestinations.labels(in: mailboxes)
        }
    }

    /// Why there is nothing to choose from.
    func nothingToChoose(account: String) -> String {
        switch self {
        case .move: "\(account) has no other folder to move to."
        case .addLabel, .removeLabel: "\(account) has no labels."
        }
    }
}

@MainActor
final class FilingPalette: NSObject {
    private static let size = NSSize(width: 420, height: 360)
    private static let margin: CGFloat = 16
    private static let badgeSize: CGFloat = 20
    private static let cellID = NSUserInterfaceItemIdentifier("FilingDestination")

    let panel: NSPanel
    let field = NSSearchField()
    let table = NSTableView()
    private(set) var shown: [Mailbox] = []

    private let mailboxes: [Mailbox]
    private let recent: [Int64]
    private let emptyLabel = NSTextField(labelWithString: "No matching mailboxes")
    private let chooseButton: NSButton
    private let cancelButton = NSButton(title: "Cancel", target: nil, action: nil)
    private var chosen: Mailbox?

    init(
        choice: FilingChoice, mailboxes: [Mailbox], recent: [Int64],
        accountName: String, account: AccountAppearance?
    ) {
        self.mailboxes = mailboxes
        self.recent = recent
        chooseButton = NSButton(title: choice.verb, target: nil, action: nil)
        panel = NSPanel(
            contentRect: NSRect(origin: .zero, size: Self.size),
            styleMask: [.titled], backing: .buffered, defer: false)
        super.init()
        panel.isReleasedWhenClosed = false
        panel.title = choice.title
        panel.setAccessibilityIdentifier("filingPalette")
        build(title: choice.title, accountName: accountName, account: account)
        filter()
    }

    private func build(title: String, accountName: String, account: AccountAppearance?) {
        let heading = NSTextField(labelWithString: title)
        heading.font = .boldSystemFont(ofSize: NSFont.systemFontSize)
        let badge = SidebarMonogramView()
        badge.account = account
        badge.setAccessibilityElement(false)
        NSLayoutConstraint.activate([
            badge.widthAnchor.constraint(equalToConstant: Self.badgeSize),
            badge.heightAnchor.constraint(equalToConstant: Self.badgeSize),
        ])
        let name = NSTextField(labelWithString: accountName)
        name.lineBreakMode = .byTruncatingTail
        name.setAccessibilityLabel("Account, \(accountName)")
        name.setAccessibilityIdentifier("filingPaletteAccount")
        let header = NSStackView(views: [heading, badge, name])

        field.delegate = self
        field.sendsSearchStringImmediately = true
        field.setAccessibilityLabel("Mailbox")
        field.setAccessibilityIdentifier("filingPaletteField")

        let column = NSTableColumn(identifier: Self.cellID)
        column.resizingMask = .autoresizingMask
        table.addTableColumn(column)
        table.headerView = nil
        table.allowsEmptySelection = true
        table.dataSource = self
        table.delegate = self
        table.target = self
        table.doubleAction = #selector(choose(_:))
        table.setAccessibilityLabel("Mailboxes")
        table.setAccessibilityIdentifier("filingPaletteList")
        let scroll = NSScrollView()
        scroll.documentView = table
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder

        emptyLabel.textColor = .secondaryLabelColor
        emptyLabel.isHidden = true

        chooseButton.target = self
        chooseButton.action = #selector(choose(_:))
        chooseButton.keyEquivalent = "\r"
        chooseButton.setAccessibilityIdentifier("filingPaletteChoose")
        cancelButton.target = self
        cancelButton.action = #selector(cancel(_:))
        cancelButton.keyEquivalent = "\u{1b}"
        let buttons = NSStackView(views: [NSView(), cancelButton, chooseButton])

        let stack = NSStackView(views: [header, field, scroll, emptyLabel, buttons])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView(frame: NSRect(origin: .zero, size: Self.size))
        content.addSubview(stack)
        panel.contentView = content
        NSLayoutConstraint.activate([
            content.widthAnchor.constraint(equalToConstant: Self.size.width),
            content.heightAnchor.constraint(equalToConstant: Self.size.height),
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: Self.margin),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -Self.margin),
            stack.topAnchor.constraint(equalTo: content.topAnchor, constant: Self.margin),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -Self.margin),
            field.widthAnchor.constraint(equalTo: stack.widthAnchor),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor),
            buttons.widthAnchor.constraint(equalTo: stack.widthAnchor),
        ])
        panel.initialFirstResponder = field
    }

    /// Show the sheet; `completion` gets the chosen mailbox, or nil when
    /// it was cancelled or ended from outside.
    func begin(on window: NSWindow, completion: @escaping @MainActor (Mailbox?) -> Void) {
        window.beginSheet(panel) { [weak self] response in
            guard let self else { return }
            panel.orderOut(nil)
            completion(response == .OK ? chosen : nil)
        }
        panel.makeFirstResponder(field)
    }

    /// List what the field's text matches, the first one chosen.
    func filter() {
        shown = FilingDestinations.ranked(mailboxes, recent: recent, query: field.stringValue)
        table.reloadData()
        table.selectRowIndexes(shown.isEmpty ? [] : [0], byExtendingSelection: false)
        emptyLabel.isHidden = !shown.isEmpty
        chooseButton.isEnabled = !shown.isEmpty
    }

    /// Up or Down from the field: the choice moves and is announced,
    /// since the keyboard stays in the field.
    func moveChoice(by step: Int) {
        guard !shown.isEmpty else { return }
        let row = min(max(table.selectedRow + step, 0), shown.count - 1)
        table.selectRowIndexes([row], byExtendingSelection: false)
        table.scrollRowToVisible(row)
        NSAccessibility.post(
            element: panel, notification: .announcementRequested,
            userInfo: [
                .announcement: shown[row].name,
                .priority: NSAccessibilityPriorityLevel.high.rawValue,
            ])
    }

    @objc func choose(_ sender: Any?) {
        let row = table.selectedRow
        guard shown.indices.contains(row) else {
            NSSound.beep()
            return
        }
        chosen = shown[row]
        panel.sheetParent?.endSheet(panel, returnCode: .OK)
    }

    @objc func cancel(_ sender: Any?) {
        panel.sheetParent?.endSheet(panel, returnCode: .cancel)
    }
}

extension FilingPalette: NSSearchFieldDelegate {
    func controlTextDidChange(_ notification: Notification) {
        filter()
    }

    func control(_ control: NSControl, textView: NSTextView, doCommandBy command: Selector) -> Bool {
        guard !textView.hasMarkedText() else { return false }
        switch command {
        case #selector(NSResponder.moveUp(_:)): moveChoice(by: -1)
        case #selector(NSResponder.moveDown(_:)): moveChoice(by: 1)
        case #selector(NSResponder.insertNewline(_:)): choose(nil)
        case #selector(NSResponder.cancelOperation(_:)): cancel(nil)
        default: return false
        }
        return true
    }
}

extension FilingPalette: NSTableViewDataSource, NSTableViewDelegate {
    func numberOfRows(in tableView: NSTableView) -> Int {
        shown.count
    }

    func tableView(_ tableView: NSTableView, viewFor column: NSTableColumn?, row: Int) -> NSView? {
        let cell = tableView.makeView(withIdentifier: Self.cellID, owner: nil) as? NSTableCellView
            ?? Self.makeCell()
        cell.textField?.stringValue = shown[row].name
        return cell
    }

    private static func makeCell() -> NSTableCellView {
        let cell = NSTableCellView()
        cell.identifier = cellID
        let text = NSTextField(labelWithString: "")
        text.lineBreakMode = .byTruncatingMiddle
        text.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(text)
        cell.textField = text
        NSLayoutConstraint.activate([
            text.leadingAnchor.constraint(equalTo: cell.leadingAnchor),
            text.trailingAnchor.constraint(equalTo: cell.trailingAnchor),
            text.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }
}
