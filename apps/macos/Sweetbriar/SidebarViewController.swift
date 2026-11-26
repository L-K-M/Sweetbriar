// The main window's sidebar (spec/08 "Sidebar"): a source-list
// `NSOutlineView` over the bridge's `SidebarModel`. Section headings
// are group rows; entries sit below them with their unread counts.
// Choosing an entry that opens a window calls `onChoose`, Drafts
// (local) `onChooseDrafts`, Out Box `onChooseOutBox`; an entry that
// opens nothing shows disabled, its note as the tooltip.

import AppKit
import Observation
import SweetbriarBridge

@MainActor
final class SidebarViewController: NSViewController, NSMenuDelegate {
    /// The outline's accessibility identifier, for UI tests.
    static let accessibilityID = "sidebar"
    private static let entryCellID = NSUserInterfaceItemIdentifier("SidebarEntry")
    private static let headerCellID = NSUserInterfaceItemIdentifier("SidebarHeader")
    private static let healthCellID = NSUserInterfaceItemIdentifier("SidebarHealth")
    private static let collapsedSectionsKey = "SidebarCollapsedSections"
    private let model: SidebarModel
    private let appearance: AppearanceModel
    private let defaults: UserDefaults
    /// The app's shared Activity model; its `accounts` drive the
    /// account health rows.
    private let activity: ActivityModel?
    private let outlineView = NSOutlineView()
    private let presentation = ActivityPresentation()
    /// Account id -> the health row's text, for the last reload; nil
    /// when the account shows no row.
    private var healthTextCache: [String: String] = [:]
    let contextMenu = NSMenu()
    private let renameItem = NSMenuItem(
        title: "Rename…", action: #selector(renameSavedViewItem(_:)), keyEquivalent: ""
    )
    private let deleteItem = NSMenuItem(
        title: "Delete", action: #selector(deleteSavedViewItem(_:)), keyEquivalent: ""
    )
    let signInAgainItem = NSMenuItem(
        title: "Sign In Again…", action: #selector(signInAgainItemChosen(_:)), keyEquivalent: ""
    )

    /// Opens the Proton sign-in sheet for an account (spec/08 "Signing
    /// in again").
    var onSignInAgain: (@MainActor (Account) -> Void)?
    /// Whether the archive takes writes now: a sign-in again writes, so
    /// a read-only archive shows it disabled, as the File menu does.
    var canWrite: @MainActor () -> Bool = { true }

    /// Asks for a new name; tests substitute a direct answer.
    var promptName: (@MainActor (@escaping (String?) -> Void) -> Void)?
    /// Asks before a delete; tests substitute a direct answer.
    var confirmDelete: (@MainActor (String, @escaping (Bool) -> Void) -> Void)?

    /// The outline's items, rebuilt per reload: NSOutlineView keys its
    /// rows by object identity.
    private var nodes: [SectionNode] = []
    private var reloadingSections = false

    /// The chosen entry's id, kept across reloads.
    private var chosenID: String?
    var selectedAccountId: String? { chosenID.flatMap { model.accountId(forEntry: $0) } }
    /// The chosen entry, for the window's title.
    var chosenEntry: SidebarModel.Entry? { chosenID.flatMap(model.entry(id:)) }
    /// The chosen entry's account, as its heading names it; nil in the
    /// unified section.
    var chosenAccountTitle: String? {
        guard let account = selectedAccountId,
            let section = model.sections.first(where: { $0.id == account })
        else { return nil }
        return Self.accountLabel(section, among: model.sections)
    }

    /// While the selection is set in code: the delegate then reports
    /// no choice.
    private var selectingInCode = false

    /// Takes rows dropped on a mailbox (spec/08 "Keyboard filing").
    weak var dropTarget: (any MessageDropTarget)?

    /// Choosing an entry that opens a window.
    var onChoose: (@MainActor (WindowSpec) -> Void)?
    /// Choosing Drafts (local) (spec/08 "Drafts (local)").
    var onChooseDrafts: (@MainActor () -> Void)?
    /// Choosing Out Box (spec/08 "Out Box").
    var onChooseOutBox: (@MainActor () -> Void)?
    /// Clicking an account's health row (spec/08 "Activity / account
    /// health").
    var onOpenActivity: (@MainActor () -> Void)?

    init(
        model: SidebarModel, appearance: AppearanceModel, activity: ActivityModel? = nil,
        defaults: UserDefaults = .standard
    ) {
        self.model = model
        self.appearance = appearance
        self.activity = activity
        self.defaults = defaults
        super.init(nibName: nil, bundle: nil)
        observeAppearance()
        observeActivity()
    }

    /// Follow the shared Activity model's accounts; each change
    /// re-arms, and only the account sections whose row text changed
    /// reload.
    private func observeActivity() {
        guard let activity else { return }
        withObservationTracking {
            _ = activity.accounts
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.updateHealthRows()
                self.observeActivity()
            }
        }
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

    /// A restore drops this controller while it is still registered
    /// for the accessibility notification it exists to observe, so a
    /// later Increase Contrast change would post to a dead object.
    deinit {
        NSWorkspace.shared.notificationCenter.removeObserver(self)
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
        outlineView.registerForDraggedTypes([MessageListDrag.pasteboardType])
        outlineView.delegate = self
        outlineView.setAccessibilityIdentifier(Self.accessibilityID)
        contextMenu.delegate = self
        contextMenu.autoenablesItems = false
        renameItem.target = self
        deleteItem.target = self
        signInAgainItem.target = self
        renameItem.identifier = MainMenuBuilder.ItemID.command("savedView.rename")
        deleteItem.identifier = MainMenuBuilder.ItemID.command("savedView.delete")
        signInAgainItem.identifier = MainMenuBuilder.ItemID.command("account.signInAgain")
        contextMenu.addItem(renameItem)
        contextMenu.addItem(deleteItem)
        contextMenu.addItem(signInAgainItem)
        outlineView.menu = contextMenu

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

    /// Keep the section disclosures and the visible selection across
    /// reloads, even though the model supplies new row objects.
    func reloadSections() {
        reloadingSections = true
        defer { reloadingSections = false }
        nodes = model.sections.map { section in
            SectionNode(section, health: healthText(for: section.account?.accountId))
        }
        for section in model.sections where section.account?.kind == .proton {
            healthTextCache[section.id] = healthText(for: section.account?.accountId)
        }
        outlineView.reloadData()
        let collapsed = Set(defaults.stringArray(forKey: Self.collapsedSectionsKey) ?? [])
        for node in nodes where !collapsed.contains(node.section.id) {
            outlineView.expandItem(node)
        }
        select(entryID: chosenID)
    }

    /// Re-read each Proton account's health row after the Activity
    /// model changed; only the sections whose row appeared, vanished
    /// or changed text reload.
    private func updateHealthRows() {
        var changed: [String] = []
        for section in model.sections {
            guard let account = section.account, account.kind == .proton else { continue }
            let text = healthText(for: account.accountId)
            if healthTextCache[account.accountId] != text {
                healthTextCache[account.accountId] = text
                changed.append(account.accountId)
            }
        }
        for accountId in changed {
            guard let index = nodes.firstIndex(where: { $0.section.id == accountId }) else {
                continue
            }
            nodes[index].updateHealth(healthTextCache[accountId])
            outlineView.reloadItem(nodes[index], reloadChildren: true)
        }
        if !changed.isEmpty { select(entryID: chosenID) }
    }

    /// The health row's entry id, stable across reloads.
    nonisolated static func healthEntryID(_ accountId: String) -> String {
        "\(accountId)/health"
    }

    /// The health row's text for `accountId`: the breaker summary when
    /// the account has open events, else the condition's title; nil
    /// for a healthy account with no events.
    private func healthText(for accountId: String?) -> String? {
        guard let accountId, let activity,
              let row = activity.accounts.first(where: { Self.sameId($0.accountId, accountId) })
        else { return nil }
        return Self.healthRowText(
            condition: row.condition, breakerEvents: row.breakerEvents,
            presentation: presentation, now: Date())
    }

    /// Account ids match by their bytes, as the chips match them: a
    /// canonically equivalent spelling is another account.
    private static func sameId(_ lhs: String, _ rhs: String) -> Bool {
        lhs.utf8.elementsEqual(rhs.utf8)
    }

    /// The row text itself, free of the sidebar's state: one line per
    /// account, summing the disappeared messages of its open events.
    static func healthRowText(
        condition: AccountCondition, breakerEvents: [AccountBreakerEvent],
        presentation: ActivityPresentation, now: Date
    ) -> String? {
        if !breakerEvents.isEmpty {
            let disappeared = breakerEvents.reduce(0) { $0 + $1.disappeared }
            return presentation.breakerEvent(AccountBreakerEvent(
                eventId: "summary", mailboxId: nil, disappeared: disappeared,
                detectedAt: now))
        }
        guard condition != .healthy else { return nil }
        return presentation.condition(condition, lastSync: nil, now: now).title
    }

    /// Rename the saved view `id`, asking first (spec/08).
    func renameSavedView(id: String) {
        let confirm: (@MainActor (String?) -> Void) = { [weak self] name in
            guard let self, let name, !name.isEmpty else { return }
            Task { @MainActor [weak self] in
                guard let self else { return }
                do {
                    try await self.model.renameView(id: id, to: name)
                    self.reloadSections()
                    self.select(entryID: SidebarModel.savedViewEntryID(id))
                } catch {
                    self.report(error)
                }
            }
        }
        if let promptName {
            promptName(confirm)
        } else {
            presentRenameSheet(for: id, confirm: confirm)
        }
    }

    /// Delete the saved view `id`, asking first.
    func deleteSavedView(id: String) {
        let name = model.savedView(id: id)?.name ?? "this view"
        let confirm: (@MainActor (Bool) -> Void) = { [weak self] accepted in
            guard let self, accepted else { return }
            Task { @MainActor [weak self] in
                guard let self else { return }
                let entryID = SidebarModel.savedViewEntryID(id)
                let wasChosen = self.chosenID == entryID
                do {
                    try await self.model.deleteView(id: id)
                    self.reloadSections()
                    if wasChosen { self.choose(entryID: SidebarModel.unifiedInboxID) }
                } catch {
                    self.report(error)
                }
            }
        }
        if let confirmDelete {
            confirmDelete(name, confirm)
        } else {
            presentDeleteSheet(named: name, confirm: confirm)
        }
    }

    /// Show a failed write as an alert sheet.
    private func report(_ error: any Error) {
        let alert = NSAlert()
        alert.messageText = error.localizedDescription
        alert.alertStyle = .warning
        if let window = view.window {
            alert.beginSheetModal(for: window) { _ in }
        } else {
            alert.runModal()
        }
    }

    /// One alert sheet for Rename and Delete; `confirm` gets the button
    /// index, 0 for the first.
    private func presentSheet(
        message: String, informative: String, buttons: [String],
        accessory: NSView?, confirm: @escaping @MainActor (Int) -> Void
    ) {
        let alert = NSAlert()
        alert.messageText = message
        alert.informativeText = informative
        alert.alertStyle = .warning
        for button in buttons { alert.addButton(withTitle: button) }
        alert.accessoryView = accessory
        let first = NSApplication.ModalResponse.alertFirstButtonReturn.rawValue
        if let window = view.window {
            alert.beginSheetModal(for: window) { confirm($0.rawValue - first) }
        } else {
            confirm(alert.runModal().rawValue - first)
        }
    }

    private func presentRenameSheet(
        for id: String, confirm: @escaping @MainActor (String?) -> Void
    ) {
        let saved = model.savedView(id: id)
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 240, height: 24))
        field.stringValue = saved?.name ?? ""
        presentSheet(
            message: "Rename View",
            informative: "Rename \"\(saved?.name ?? "this view")\".",
            buttons: ["Rename", "Cancel"],
            accessory: field
        ) { confirm($0 == 0 ? field.stringValue : nil) }
    }

    private func presentDeleteSheet(
        named name: String, confirm: @escaping @MainActor (Bool) -> Void
    ) {
        presentSheet(
            message: "Delete View?",
            informative: "Delete \"\(name)\"? This cannot be undone.",
            buttons: ["Delete", "Cancel"],
            accessory: nil
        ) { confirm($0 == 0) }
    }

    /// Choose the entry `id` names, as a click would: it is selected
    /// and, if it opens a window, `onChoose` hears of it.
    func choose(entryID id: String, revealSection: Bool = true) {
        guard let entry = model.entry(id: id), Self.isChoosable(entry) else { return }
        chosenID = id
        select(entryID: id, revealSection: revealSection)
        report(entry)
    }

    private static func isChoosable(_ entry: SidebarModel.Entry) -> Bool {
        switch entry.target {
        case .window, .drafts, .outBox: return true
        case .unavailable: return false
        }
    }

    private func report(_ entry: SidebarModel.Entry) {
        switch entry.target {
        case .window(let spec): onChoose?(spec)
        case .drafts: onChooseDrafts?()
        case .outBox: onChooseOutBox?()
        case .unavailable: break
        }
    }

    /// Select the row of `id` without reporting a choice.
    private func select(entryID id: String?, revealSection: Bool = false) {
        selectingInCode = true
        defer { selectingInCode = false }
        guard let id, let (section, node) = entryNode(id: id) else {
            outlineView.deselectAll(nil)
            return
        }
        if revealSection { outlineView.expandItem(section) }
        let row = outlineView.row(forItem: node)
        guard row >= 0 else { return }
        outlineView.selectRowIndexes(IndexSet(integer: row), byExtendingSelection: false)
        // The section may have been expanded just now, so the row can
        // sit below the fold: Mailbox → <name> (⌘1–⌘4) has to show
        // what it chose, not only select it.
        outlineView.scrollRowToVisible(row)
    }

    private func entryNode(id: String) -> (SectionNode, EntryNode)? {
        for section in nodes {
            if let node = section.entries.first(where: { $0.entry.id == id }) {
                return (section, node)
            }
        }
        return nil
    }

    private func recordDisclosure(_ notification: Notification, expanded: Bool) {
        guard !reloadingSections, let node = notification.userInfo?["NSObject"] as? SectionNode else { return }
        var collapsed = Set(defaults.stringArray(forKey: Self.collapsedSectionsKey) ?? [])
        if expanded {
            collapsed.remove(node.section.id)
            if !selectingInCode { select(entryID: chosenID) }
        } else {
            collapsed.insert(node.section.id)
        }
        defaults.set(collapsed.sorted(), forKey: Self.collapsedSectionsKey)
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

    func outlineViewItemDidExpand(_ notification: Notification) {
        recordDisclosure(notification, expanded: true)
    }

    func outlineViewItemDidCollapse(_ notification: Notification) {
        recordDisclosure(notification, expanded: false)
    }

    func outlineView(_ outlineView: NSOutlineView, heightOfRowByItem item: Any) -> CGFloat {
        if item is SectionNode { return 24 }
        if let node = item as? EntryNode, node.isHealth { return 18 }
        return outlineView.rowHeight
    }

    /// Rows of this window's list may be dropped on any mailbox, which
    /// files them or says why it can't; nothing else takes a drop.
    func outlineView(
        _ outlineView: NSOutlineView, validateDrop info: any NSDraggingInfo,
        proposedItem item: Any?, proposedChildIndex index: Int
    ) -> NSDragOperation {
        guard let item, dropMailbox(for: item) != nil,
            dropTarget?.takesDrop(from: info.draggingSource) == true
        else { return [] }
        outlineView.setDropItem(item, dropChildIndex: NSOutlineViewDropOnItemIndex)
        return .move
    }

    func outlineView(
        _ outlineView: NSOutlineView, acceptDrop info: any NSDraggingInfo,
        item: Any?, childIndex index: Int
    ) -> Bool {
        guard let item, let mailbox = dropMailbox(for: item) else { return false }
        return dropTarget?.drop(from: info.draggingSource, onto: mailbox) == true
    }

    private func dropMailbox(for item: Any) -> Mailbox? {
        guard let node = item as? EntryNode, !node.isHealth else { return nil }
        return model.mailbox(forEntry: node.entry.id)
    }

    /// Only entries that open a window or the drafts list can be
    /// selected (spec/08: a mailbox that isn't selectable opens no window).
    func outlineView(_ outlineView: NSOutlineView, shouldSelectItem item: Any) -> Bool {
        guard let node = item as? EntryNode else { return false }
        return Self.isChoosable(node.entry)
    }

    func outlineView(
        _ outlineView: NSOutlineView,
        viewFor tableColumn: NSTableColumn?,
        item: Any
    ) -> NSView? {
        if let section = item as? SectionNode {
            let cell = outlineView.makeView(withIdentifier: Self.headerCellID, owner: nil)
                as? SidebarHeaderCell ?? SidebarHeaderCell(identifier: Self.headerCellID)
            let account = appearance.appearance?.accounts.first {
                Self.sameId($0.accountId, section.section.id)
            }
            let label = Self.accountLabel(section.section, among: model.sections)
            cell.configure(title: label, account: account, isAccount: section.section.id != SidebarModel.unifiedSectionID)
            return cell
        }
        guard let node = item as? EntryNode else { return nil }
        if node.isHealth {
            let cell = outlineView.makeView(withIdentifier: Self.healthCellID, owner: nil)
                as? SidebarHealthCellView ?? SidebarHealthCellView(identifier: Self.healthCellID)
            cell.configure(text: node.entry.title) { [weak self] in
                self?.onOpenActivity?()
            }
            return cell
        }
        let cell = outlineView.makeView(withIdentifier: Self.entryCellID, owner: nil)
            as? SidebarEntryCellView ?? SidebarEntryCellView(identifier: Self.entryCellID)
        cell.configure(node.entry, symbol: SidebarSymbol.name(for: node.entry, mailbox: model.mailbox(forEntry: node.entry.id)))
        return cell
    }

    /// Only a saved view's row offers Rename and Delete (spec/08); only
    /// a Proton account's heading and mailboxes offer Sign In Again.
    func menuNeedsUpdate(_ menu: NSMenu) {
        guard menu === contextMenu else { return }
        let row = outlineView.clickedRow >= 0 ? outlineView.clickedRow : outlineView.selectedRow
        var viewId: String?
        var rowId: String?
        if row >= 0, let node = outlineView.item(atRow: row) as? EntryNode {
            rowId = node.entry.id
            if case .window(.view(let id)) = node.entry.target {
                viewId = id
            }
        } else if row >= 0, let node = outlineView.item(atRow: row) as? SectionNode {
            rowId = node.section.id
        }
        updateMenu(viewId: viewId, rowId: rowId)
    }

    /// Fit the context menu to a row: its saved view, if any, and the
    /// Proton account it belongs to.
    func updateMenu(viewId: String?, rowId: String?) {
        renameItem.isEnabled = viewId != nil
        deleteItem.isEnabled = viewId != nil
        renameItem.representedObject = viewId
        deleteItem.representedObject = viewId
        let proton = rowId.flatMap(model.protonAccount(forRow:))
        signInAgainItem.isHidden = proton == nil
        signInAgainItem.isEnabled = proton != nil && onSignInAgain != nil && canWrite()
        signInAgainItem.representedObject = proton?.accountId
    }

    @objc private func renameSavedViewItem(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        renameSavedView(id: id)
    }

    @objc private func deleteSavedViewItem(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String else { return }
        deleteSavedView(id: id)
    }

    @objc private func signInAgainItemChosen(_ sender: NSMenuItem) {
        guard let id = sender.representedObject as? String,
              let account = model.protonAccount(forRow: id)
        else { return }
        onSignInAgain?(account)
    }

    func outlineViewSelectionDidChange(_ notification: Notification) {
        guard !selectingInCode, !reloadingSections else { return }
        let row = outlineView.selectedRow
        guard row >= 0, let node = outlineView.item(atRow: row) as? EntryNode,
              Self.isChoosable(node.entry)
        else {
            return
        }
        chosenID = node.entry.id
        report(node.entry)
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
        let options = contrastOptions()
        if let logo = AccountLogoImages.image(for: account) {
            AccountLogoImages.draw(
                logo, in: bounds.insetBy(dx: 1, dy: 1), ring: Self.color(variant.markerSrgb),
                ringWidth: AccountLogoImages.ringWidth(
                    increaseContrast: options.increaseContrast, differentiate: options.differentiate))
            return
        }
        let shape = NSBezierPath(ovalIn: bounds.insetBy(dx: 1, dy: 1))
        Self.color(variant.stripSrgb).setFill()
        shape.fill()
        let appearanceName = appearance?.name ?? effectiveAppearance.name
        let highContrast = appearanceName == .accessibilityHighContrastAqua
            || appearanceName == .accessibilityHighContrastDarkAqua
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
    var entries: [EntryNode]

    init(_ section: SidebarModel.Section, health: String?) {
        self.section = section
        self.entries = []
        super.init()
        updateHealth(health)
    }

    /// The account's mailboxes, with its health row first when it has
    /// one.
    func updateHealth(_ text: String?) {
        var entries = section.entries.map { EntryNode($0) }
        if let text, section.account?.kind == .proton {
            entries.insert(EntryNode.health(accountId: section.id, text: text), at: 0)
        }
        self.entries = entries
    }
}

/// An entry as an outline item.
private final class EntryNode: NSObject {
    let entry: SidebarModel.Entry
    /// True for the synthetic account health row, which no select
    /// should change (spec/08).
    let isHealth: Bool

    init(_ entry: SidebarModel.Entry, isHealth: Bool = false) {
        self.entry = entry
        self.isHealth = isHealth
    }

    static func health(accountId: String, text: String) -> EntryNode {
        EntryNode(
            SidebarModel.Entry(
                id: SidebarViewController.healthEntryID(accountId),
                title: text, unread: nil, target: .unavailable(note: text)),
            isHealth: true)
    }
}

/// The symbol a sidebar entry shows, as Mail's sidebar does: the
/// mailbox's role, else its kind.
enum SidebarSymbol {
    static func name(for entry: SidebarModel.Entry, mailbox: Mailbox?) -> String {
        switch entry.target {
        case .drafts: return "doc"
        case .outBox: return "tray.and.arrow.up"
        case .window(.flagged): return "flag"
        case .window(.unified(let role)): return name(for: role)
        case .window(.view): return "folder.badge.gearshape"
        case .window(.search): return "magnifyingglass"
        case .window(.thread): return "bubble.left.and.bubble.right"
        case .window(.debugSynthetic): return "hammer"
        case .window(.mailbox), .unavailable: break
        }
        guard let mailbox else { return "folder" }
        if let role = mailbox.role { return name(for: role) }
        switch mailbox.kind {
        case .label: return "tag"
        case .folder: return "folder"
        case .virtual: return "tray.full"
        }
    }

    static func name(for role: MailboxRole) -> String {
        switch role {
        case .inbox: "tray"
        case .sent: "paperplane"
        case .drafts: "doc"
        case .trash: "trash"
        case .junk: "xmark.bin"
        case .archive: "archivebox"
        case .all: "tray.full"
        case .flagged: "star"
        }
    }
}

/// An entry's row: its symbol, its title, and its unread count when it
/// has one. An entry that opens nothing is dimmed, with its note as
/// tooltip.
@MainActor
private final class SidebarEntryCellView: NSTableCellView {
    private let title = NSTextField(labelWithString: "")
    private let unread = NSTextField(labelWithString: "")
    private let unreadBadge = NSBox()
    private let symbol = NSImageView()

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        for field in [title, unread] {
            field.translatesAutoresizingMaskIntoConstraints = false
            field.lineBreakMode = .byTruncatingTail
        }
        addSubview(title)
        unreadBadge.translatesAutoresizingMaskIntoConstraints = false
        unreadBadge.boxType = .custom
        unreadBadge.borderWidth = 0
        unreadBadge.cornerRadius = 8
        unreadBadge.fillColor = .quaternaryLabelColor
        unreadBadge.setAccessibilityElement(false)
        unread.setAccessibilityElement(true)
        addSubview(unreadBadge)
        addSubview(unread)
        textField = title
        // The source list tints a cell's image view with the accent
        // color, as Mail's mailbox symbols are.
        symbol.translatesAutoresizingMaskIntoConstraints = false
        symbol.symbolConfiguration = NSImage.SymbolConfiguration(scale: .medium)
        symbol.setContentHuggingPriority(.required, for: .horizontal)
        addSubview(symbol)
        imageView = symbol
        unread.textColor = .secondaryLabelColor
        unread.alignment = .center
        unread.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .medium)
        unread.setContentCompressionResistancePriority(.required, for: .horizontal)
        title.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        NSLayoutConstraint.activate([
            symbol.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            symbol.centerYAnchor.constraint(equalTo: centerYAnchor),
            symbol.widthAnchor.constraint(equalToConstant: 20),
            title.leadingAnchor.constraint(equalTo: symbol.trailingAnchor, constant: 6),
            title.centerYAnchor.constraint(equalTo: centerYAnchor),
            unreadBadge.leadingAnchor.constraint(
                greaterThanOrEqualTo: title.trailingAnchor, constant: 6
            ),
            unreadBadge.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            unreadBadge.centerYAnchor.constraint(equalTo: centerYAnchor),
            unreadBadge.heightAnchor.constraint(equalToConstant: 16),
            unread.leadingAnchor.constraint(equalTo: unreadBadge.leadingAnchor, constant: 5),
            unread.trailingAnchor.constraint(equalTo: unreadBadge.trailingAnchor, constant: -5),
            unread.centerYAnchor.constraint(equalTo: unreadBadge.centerYAnchor),
        ])
    }

    required init?(coder: NSCoder) {
        fatalError("SidebarEntryCellView is built in code")
    }

    func configure(_ entry: SidebarModel.Entry, symbol name: String) {
        title.stringValue = entry.title
        symbol.image = NSImage(systemSymbolName: name, accessibilityDescription: nil)
        let count = entry.unread ?? 0
        let hasUnread = count > 0
        unread.stringValue = hasUnread ? count.formatted() : ""
        unread.isHidden = !hasUnread
        unreadBadge.isHidden = !hasUnread
        switch entry.target {
        case .window, .drafts, .outBox:
            title.textColor = .labelColor
            symbol.alphaValue = 1
            toolTip = nil
        case .unavailable(let note):
            title.textColor = .tertiaryLabelColor
            symbol.alphaValue = 0.4
            toolTip = note
        }
        setAccessibilityLabel(count > 0 ? "\(entry.title), \(count) unread" : entry.title)
    }
}

/// An account's health row (spec/08 "Activity / account health"): a
/// small button, so the non-selectable row still opens the Activity
/// window when clicked.
@MainActor
private final class SidebarHealthCellView: NSTableCellView {
    private let button = NSButton(title: "", target: nil, action: nil)
    private var onOpen: (@MainActor () -> Void)?

    init(identifier: NSUserInterfaceItemIdentifier) {
        super.init(frame: .zero)
        self.identifier = identifier
        button.font = .systemFont(ofSize: 11)
        button.controlSize = .small
        button.bezelStyle = .inline
        button.target = self
        button.action = #selector(openActivity(_:))
        button.translatesAutoresizingMaskIntoConstraints = false
        addSubview(button)
        NSLayoutConstraint.activate([
            button.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 2),
            button.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -4),
            button.topAnchor.constraint(equalTo: topAnchor),
            button.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    required init?(coder: NSCoder) {
        fatalError("SidebarHealthCellView is built in code")
    }

    func configure(text: String, onOpen: @escaping @MainActor () -> Void) {
        button.title = text
        button.setAccessibilityLabel(text)
        self.onOpen = onOpen
    }

    @objc private func openActivity(_ sender: Any?) { onOpen?() }
}
