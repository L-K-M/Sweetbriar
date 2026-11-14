// The main window's sidebar (spec/08 "Sidebar"): a source-list
// `NSOutlineView` over the bridge's `SidebarModel`. Section headings
// are group rows; entries sit below them with their unread counts.
// Choosing an entry that opens a window calls `onChoose`; an entry
// that opens nothing shows disabled, its note as the tooltip.

import AppKit
import Observation
import SweetbriarBridge

@MainActor
final class SidebarViewController: NSViewController {
    /// The outline's accessibility identifier, for UI tests.
    static let accessibilityID = "sidebar"
    private static let entryCellID = NSUserInterfaceItemIdentifier("SidebarEntry")
    private static let headerCellID = NSUserInterfaceItemIdentifier("SidebarHeader")
    private let model: SidebarModel
    private let appearance: AppearanceModel
    private let outlineView = NSOutlineView()

    /// The outline's items, rebuilt per reload: NSOutlineView keys its
    /// rows by object identity.
    private var nodes: [SectionNode] = []

    /// The chosen entry's id, kept across reloads.
    private var chosenID: String?
    var selectedAccountId: String? { chosenID.flatMap { model.accountId(forEntry: $0) } }

    /// While the selection is set in code: the delegate then reports
    /// no choice.
    private var selectingInCode = false

    /// Choosing an entry that opens a window.
    var onChoose: (@MainActor (WindowSpec) -> Void)?

    init(model: SidebarModel, appearance: AppearanceModel) {
        self.model = model
        self.appearance = appearance
        super.init(nibName: nil, bundle: nil)
        observeAppearance()
    }

    private func observeAppearance() {
        withObservationTracking {
            _ = appearance.appearance
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                if self.isViewLoaded { self.reloadSections() }
                self.observeAppearance()
            }
        }
    }

    required init?(coder: NSCoder) {
        fatalError("SidebarViewController is built in code")
    }

    override func loadView() {
        NSWorkspace.shared.notificationCenter.addObserver(
            self, selector: #selector(accessibilityOptionsChanged),
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil
        )
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

    @objc private func accessibilityOptionsChanged() {
        reloadSections()
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

    func outlineView(_ outlineView: NSOutlineView, heightOfRowByItem item: Any) -> CGFloat {
        item is SectionNode ? 24 : outlineView.rowHeight
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
                as? SidebarHeaderCell ?? SidebarHeaderCell(identifier: Self.headerCellID)
            let account = appearance.appearance?.accounts.first { $0.accountId == section.section.id }
            let label = Self.accountLabel(section.section, among: model.sections)
            cell.configure(title: label, account: account, isAccount: section.section.id != SidebarModel.unifiedSectionID)
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

    static func accountLabel(_ section: SidebarModel.Section, among sections: [SidebarModel.Section]) -> String {
        guard section.id != SidebarModel.unifiedSectionID else { return section.title }
        let accounts = sections.filter { $0.id != SidebarModel.unifiedSectionID }.sorted { $0.id < $1.id }
        guard let sectionIndex = accounts.firstIndex(where: { $0.id == section.id }) else { return section.title }
        let titleCounts = Dictionary(grouping: accounts, by: \.title).mapValues(\.count)
        var labels = accounts.map { account in
            titleCounts[account.title, default: 0] > 1 ? "\(account.title) · \(account.id)" : account.title
        }
        // A generated label can itself be another account's literal name.
        // Bound the retries, then use unique ordinal prefixes for every account.
        for _ in 0..<accounts.count {
            let counts = Dictionary(grouping: labels, by: { $0 }).mapValues(\.count)
            if counts.values.allSatisfy({ $0 == 1 }) {
                return labels[sectionIndex]
            }
            for index in accounts.indices where counts[labels[index], default: 0] > 1 {
                labels[index] += " · \(accounts[index].id)"
            }
        }
        return "\(sectionIndex + 1). \(section.title)"
    }
}

// MARK: - Items and cells

@MainActor
final class SidebarHeaderCell: NSTableCellView {
    private(set) var badge = SidebarMonogramView()
    private let label = NSTextField(labelWithString: "")

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        badge.translatesAutoresizingMaskIntoConstraints = false
        badge.setAccessibilityElement(false)
        label.translatesAutoresizingMaskIntoConstraints = false
        let row = NSStackView(views: [badge, label])
        row.orientation = .horizontal
        row.spacing = 4
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        textField = label
        NSLayoutConstraint.activate([
            badge.widthAnchor.constraint(equalToConstant: 20),
            badge.heightAnchor.constraint(equalToConstant: 20),
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            row.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor),
            row.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) { fatalError("SidebarHeaderCell is built in code") }

    func configure(title: String, account: AccountAppearance?, isAccount: Bool) {
        label.stringValue = title
        badge.isHidden = !isAccount
        badge.account = account
        setAccessibilityLabel(isAccount ? "Account, \(title)" : title)
    }
}

@MainActor
final class SidebarMonogramView: NSView {
    var contrastOptions: () -> (increaseContrast: Bool, differentiate: Bool) = {
        (NSWorkspace.shared.accessibilityDisplayShouldIncreaseContrast,
         NSWorkspace.shared.accessibilityDisplayShouldDifferentiateWithoutColor)
    }
    var account: AccountAppearance? { didSet { needsDisplay = true } }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        guard let account else {
            NSImage(systemSymbolName: "person.crop.circle", accessibilityDescription: nil)?
                .draw(in: bounds)
            return
        }
        let dark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let variant = dark ? account.tokens.dark : account.tokens.light
        let shape = NSBezierPath(ovalIn: bounds.insetBy(dx: 1, dy: 1))
        Self.color(variant.stripSrgb).setFill()
        shape.fill()
        let appearanceName = appearance?.name ?? effectiveAppearance.name
        let highContrast = appearanceName == .accessibilityHighContrastAqua
            || appearanceName == .accessibilityHighContrastDarkAqua
        let options = contrastOptions()
        if highContrast || options.increaseContrast || options.differentiate {
            Self.color(variant.outlineSrgb).setStroke()
            shape.lineWidth = 1.5
            shape.stroke()
        }
        let attributes: [NSAttributedString.Key: Any] = [
            .font: NSFont.boldSystemFont(ofSize: 11),
            .foregroundColor: Self.color(variant.markerSrgb),
        ]
        let text = account.monogram as NSString
        let size = text.size(withAttributes: attributes)
        text.draw(at: NSPoint(x: (bounds.width - size.width) / 2, y: (bounds.height - size.height) / 2),
                  withAttributes: attributes)
    }

    private static func color(_ hex: String) -> NSColor {
        guard hex.count == 7, hex.first == "#", let value = UInt32(hex.dropFirst(), radix: 16) else {
            return .labelColor
        }
        return NSColor(srgbRed: CGFloat((value >> 16) & 255) / 255,
                       green: CGFloat((value >> 8) & 255) / 255,
                       blue: CGFloat(value & 255) / 255, alpha: 1)
    }
}

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
