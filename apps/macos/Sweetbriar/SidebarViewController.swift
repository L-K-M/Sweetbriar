// The main window's sidebar (spec/08 "Sidebar"): a source-list
// `NSOutlineView` over the bridge's `SidebarModel`. Section headings
// are group rows; entries sit below them with their unread counts.
// Choosing an entry that opens a window calls `onChoose`; an entry
// that opens nothing shows disabled, its note as the tooltip.

import AppKit
import SweetbriarBridge

@MainActor
final class SidebarViewController: NSViewController {
    /// The outline's accessibility identifier, for UI tests.
    static let accessibilityID = "sidebar"
    private static let entryCellID = NSUserInterfaceItemIdentifier("SidebarEntry")
    private static let headerCellID = NSUserInterfaceItemIdentifier("SidebarHeader")
    /// An account's neutral symbol before M1.10's appearance service.
    private static let accountSymbol = NSImage(
        systemSymbolName: "person.crop.circle",
        accessibilityDescription: nil
    )

    private let model: SidebarModel
    private let outlineView = NSOutlineView()

    /// The outline's items, rebuilt per reload: NSOutlineView keys its
    /// rows by object identity.
    private var nodes: [SectionNode] = []

    /// The chosen entry's id, kept across reloads.
    private var chosenID: String?

    /// While the selection is set in code: the delegate then reports
    /// no choice.
    private var selectingInCode = false

    /// Choosing an entry that opens a window.
    var onChoose: (@MainActor (WindowSpec) -> Void)?

    init(model: SidebarModel) {
        self.model = model
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) {
        fatalError("SidebarViewController is built in code")
    }

    override func loadView() {
        let column = NSTableColumn(identifier: NSUserInterfaceItemIdentifier("entries"))
        column.resizingMask = .autoresizingMask
        outlineView.addTableColumn(column)
        outlineView.outlineTableColumn = column
        outlineView.headerView = nil
        outlineView.style = .sourceList
        outlineView.floatsGroupRows = false
        outlineView.allowsEmptySelection = true
        outlineView.dataSource = self
        outlineView.delegate = self
        outlineView.setAccessibilityIdentifier(Self.accessibilityID)

        let scrollView = NSScrollView()
        scrollView.documentView = outlineView
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.drawsBackground = false
        view = scrollView
    }

    /// Show the model's sections, every one expanded, keeping the
    /// chosen entry selected while it is still listed.
    func reloadSections() {
        nodes = model.sections.map(SectionNode.init)
        outlineView.reloadData()
        for node in nodes {
            outlineView.expandItem(node)
        }
        select(entryID: chosenID)
    }

    /// Choose the entry `id` names, as a click would: it is selected
    /// and, if it opens a window, `onChoose` hears of it.
    func choose(entryID id: String) {
        guard let entry = model.entry(id: id), case .window(let spec) = entry.target else {
            return
        }
        chosenID = id
        select(entryID: id)
        onChoose?(spec)
    }

    /// Select the row of `id` without reporting a choice.
    private func select(entryID id: String?) {
        selectingInCode = true
        defer { selectingInCode = false }
        guard let id, let node = entryNode(id: id) else {
            outlineView.deselectAll(nil)
            return
        }
        let row = outlineView.row(forItem: node)
        guard row >= 0 else { return }
        outlineView.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
    }

    private func entryNode(id: String) -> EntryNode? {
        for section in nodes {
            if let node = section.entries.first(where: { $0.entry.id == id }) {
                return node
            }
        }
        return nil
    }
}

// MARK: - Outline data source and delegate

extension SidebarViewController: NSOutlineViewDataSource, NSOutlineViewDelegate {
    func outlineView(_ outlineView: NSOutlineView, numberOfChildrenOfItem item: Any?) -> Int {
        switch item {
        case nil:
            return nodes.count
        case let section as SectionNode:
            return section.entries.count
        default:
            return 0
        }
    }

    func outlineView(_ outlineView: NSOutlineView, child index: Int, ofItem item: Any?) -> Any {
        if let section = item as? SectionNode {
            return section.entries[index]
        }
        return nodes[index]
    }

    func outlineView(_ outlineView: NSOutlineView, isItemExpandable item: Any) -> Bool {
        item is SectionNode
    }

    func outlineView(_ outlineView: NSOutlineView, isGroupItem item: Any) -> Bool {
        item is SectionNode
    }

    /// Only entries that open a window can be selected (spec/08: a
    /// mailbox that isn't selectable opens no window).
    func outlineView(_ outlineView: NSOutlineView, shouldSelectItem item: Any) -> Bool {
        guard let node = item as? EntryNode, case .window = node.entry.target else {
            return false
        }
        return true
    }

    func outlineView(
        _ outlineView: NSOutlineView,
        viewFor tableColumn: NSTableColumn?,
        item: Any
    ) -> NSView? {
        if let section = item as? SectionNode {
            let cell = outlineView.makeView(withIdentifier: Self.headerCellID, owner: nil)
                as? NSTableCellView ?? makeHeaderCell()
            cell.textField?.stringValue = section.section.title
            // Accounts carry a neutral symbol until M1.10 gives them
            // colors and monograms (spec/08 "Sidebar").
            let isAccount = section.section.id != SidebarModel.unifiedSectionID
            cell.imageView?.image = isAccount ? Self.accountSymbol : nil
            cell.imageView?.isHidden = !isAccount
            return cell
        }
        guard let node = item as? EntryNode else { return nil }
        let cell = outlineView.makeView(withIdentifier: Self.entryCellID, owner: nil)
            as? SidebarEntryCellView ?? SidebarEntryCellView(identifier: Self.entryCellID)
        cell.configure(node.entry)
        return cell
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !selectingInCode else { return }
        let row = outlineView.selectedRow
        guard row >= 0, let node = outlineView.item(atRow: row) as? EntryNode,
              case .window(let spec) = node.entry.target
        else {
            return
        }
        chosenID = node.entry.id
        onChoose?(spec)
    }

    private func makeHeaderCell() -> NSTableCellView {
        let cell = NSTableCellView()
        cell.identifier = Self.headerCellID
        let symbol = NSImageView()
        symbol.translatesAutoresizingMaskIntoConstraints = false
        symbol.contentTintColor = .secondaryLabelColor
        symbol.setAccessibilityElement(false)
        let label = NSTextField(labelWithString: "")
        label.translatesAutoresizingMaskIntoConstraints = false
        let row = NSStackView(views: [symbol, label])
        row.orientation = .horizontal
        row.spacing = 4
        row.translatesAutoresizingMaskIntoConstraints = false
        cell.addSubview(row)
        cell.imageView = symbol
        cell.textField = label
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: cell.leadingAnchor, constant: 2),
            row.trailingAnchor.constraint(lessThanOrEqualTo: cell.trailingAnchor),
            row.centerYAnchor.constraint(equalTo: cell.centerYAnchor),
        ])
        return cell
    }
}

// MARK: - Items and cells

/// A section as an outline item.
private final class SectionNode: NSObject {
    let section: SidebarModel.Section
    let entries: [EntryNode]

    init(_ section: SidebarModel.Section) {
        self.section = section
        entries = section.entries.map(EntryNode.init)
    }
}

/// An entry as an outline item.
private final class EntryNode: NSObject {
    let entry: SidebarModel.Entry

    init(_ entry: SidebarModel.Entry) {
        self.entry = entry
    }
}

/// An entry's row: its title, and its unread count when it has one.
/// An entry that opens nothing is dimmed, with its note as tooltip.
@MainActor
private final class SidebarEntryCellView: NSTableCellView {
    private let title = NSTextField(labelWithString: "")
    private let unread = NSTextField(labelWithString: "")

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        for field in [title, unread] {
            field.translatesAutoresizingMaskIntoConstraints = false
            field.lineBreakMode = .byTruncatingTail
            addSubview(field)
        }
        textField = title
        unread.textColor = .secondaryLabelColor
        unread.alignment = .right
        unread.setContentCompressionResistancePriority(.required, for: .horizontal)
        title.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        NSLayoutConstraint.activate([
            title.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            title.centerYAnchor.constraint(equalTo: centerYAnchor),
            unread.leadingAnchor.constraint(
                greaterThanOrEqualTo: title.trailingAnchor, constant: 6
            ),
            unread.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            unread.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) {
        fatalError("SidebarEntryCellView is built in code")
    }

    func configure(_ entry: SidebarModel.Entry) {
        title.stringValue = entry.title
        let count = entry.unread ?? 0
        unread.stringValue = count > 0 ? "\(count)" : ""
        unread.isHidden = count == 0
        switch entry.target {
        case .window:
            title.textColor = .labelColor
            toolTip = nil
        case .unavailable(let note):
            title.textColor = .tertiaryLabelColor
            toolTip = note
        }
        setAccessibilityLabel(count > 0 ? "\(entry.title), \(count) unread" : entry.title)
    }
}
