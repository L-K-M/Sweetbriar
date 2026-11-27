// The AppKit list (plan M0.10, the production list since M1.4): a
// view-based `NSTableView` of fixed-height cells with no Auto Layout
// inside them, driven by the bridge's `ListWindowModel`. Pages land
// through `onPagesDelivered` and reload only the delivered pages'
// visible rows; the clip view's bounds changes drive `setVisible`.
// Selection is held by occurrence id, in the model too, so a 10k-row
// selection fetches nothing and follows its messages across indexes.

import AppKit
import Observation
import SweetbriarBridge

/// The AppKit list: a scroll view plus a one-column table. `init`
/// takes the callback the window uses to refresh its own chrome when
/// the id index is replaced — the table's reload is the list's own
/// business, the empty-state hint is the window's.
@MainActor
final class AppKitPrototypeList: NSView {
    /// The reuse identifier the cells are made and found under.
    private static let cellIdentifier = NSUserInterfaceItemIdentifier(
        "MessageRow"
    )
    /// The account palette; the SwiftUI list declares the same colors
    /// in the same order so `accountColorIndex` matches across lists.
    private static let accountPalette: [NSColor] = [
        .systemBlue, .systemGreen, .systemOrange,
        .systemPurple, .systemPink, .systemTeal,
    ]

    /// Whether rows show the account marker — the rail and monogram.
    enum AccountMarker {
        case shown
        /// Single-account windows need no account marker.
        case hidden
    }

    /// Where the list goes when a new index drops the whole
    /// selection.
    enum SelectionLoss {
        case keepPosition
        /// Back to the first row: Search does this once the selected
        /// message is no longer a hit (spec/08).
        case reopenAtTop
    }

    /// The table's accessibility identifier, for UI tests.
    static let accessibilityID = "messageList"

    private let model: ListWindowModel
    private let formatter: MessageRowFormatter
    private let accountMarker: AccountMarker
    private let appearanceModel: AppearanceModel?
    private let selectionLoss: SelectionLoss
    private let scrollView = NSScrollView()
    private let tableView = MessageTableView()

    /// The selection, by occurrence id, whenever it changes — the
    /// list's own clicks and a new index's changes alike.
    var onSelectionChange: (@MainActor (Set<Int64>) -> Void)?

    /// Opens a row's message in its own window: Return with one row
    /// selected, or a double-click on a row (spec/08 "Message window").
    var onOpen: (@MainActor (Int64) -> Void)?

    /// Space and ⇧Space: page the reader (spec/08 "Message window").
    var onPage: (@MainActor (ReaderPage) -> Void)?

    /// The commands the context menu offers for the rows it targets;
    /// without them, as in the prototype, its items are no-ops.
    var contextActions: (@MainActor (Set<Int64>) -> [MessageContextAction])?

    /// The rows the last drag from this list took.
    private(set) var draggedIDs: Set<Int64> = []

    /// The account label per account id, read once per reload rather
    /// than once per visible row on every reload.
    private var accountLabels: [String: String] = [:]

    /// The labels the caller last supplied, so an unchanged set is a
    /// no-op.
    private var suppliedLabels: [String: String] = [:]

    /// Where this list's filing undo is kept, apart from the window's,
    /// which the text editors share: ⌘Z in those never moves mail
    /// (spec/08 "Keyboard filing").
    var filingUndoManager: UndoManager?

    /// The table finds this through the responder chain.
    override var undoManager: UndoManager? {
        filingUndoManager ?? super.undoManager
    }

    /// True while the table's selection is set from the ids, not by the
    /// user: its callbacks then must not overwrite the model's ids — a
    /// new index may lack one the model is still following (spec/08).
    private var restoringSelection = false

    /// The selection as occurrence ids — the source of truth the
    /// table's `selectedRowIndexes` is mapped through.
    private var selectedOccurrenceIDs: Set<Int64> = []

    /// The first selected row's rank, while rows are selected.
    private var selectionRank: Int?
    /// That rank once every selected row left the list: where the next
    /// arrow continues (spec/08 "Keyboard filing").
    private var resumeRank: Int?

    init(
        model: ListWindowModel,
        formatter: MessageRowFormatter,
        accountMarker: AccountMarker = .shown,
        appearance: AppearanceModel? = nil,
        accountLabels: [String: String] = [:],
        selectionLoss: SelectionLoss = .keepPosition,
        onIndexChange: (@MainActor () -> Void)? = nil
    ) {
        self.model = model
        self.formatter = formatter
        self.accountMarker = accountMarker
        self.appearanceModel = appearance
        self.accountLabels = accountLabels
        self.selectionLoss = selectionLoss
        super.init(frame: .zero)
        buildTable()
        observeClipView()
        observeAppearance()

        // Pages land at most once per frame; reload just the rows of
        // the delivered pages that are on screen (spec/08).
        model.onPagesDelivered = { [weak self] pages in
            self?.pagesDidLand(pages)
        }
        model.onIndexChanged = { [weak self] in
            self?.indexDidChange()
            onIndexChange?()
        }
        // A new index followed or dropped selected ids (spec/08).
        model.onSelectionChanged = { [weak self] ids in
            guard let self else { return }
            let lost = ids.isEmpty && !selectedOccurrenceIDs.isEmpty
            if lost { resumeRank = selectionRank }
            selectedOccurrenceIDs = ids
            restoreSelection()
            if lost, selectionLoss == .reopenAtTop {
                scrollToRank(0)
            }
            onSelectionChange?(ids)
        }
    }

    private func observeAppearance() {
        guard let appearanceModel else { return }
        withObservationTracking {
            _ = appearanceModel.appearance
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                restoringSelection = true
                tableView.reloadData()
                restoreSelection()
                restoringSelection = false
                observeAppearance()
            }
        }
    }

    /// The caller reports that an account label changed. The list does
    /// not observe the sidebar itself: `SidebarModel.sections` is
    /// replaced on every list-generation change, so subscribing here
    /// would rebuild the whole table whenever mail arrives (spec/08
    /// "Sidebar"). Nothing happens while no label actually changed.
    func reloadAccountLabels(_ labels: [String: String]) {
        guard labels != suppliedLabels else { return }
        suppliedLabels = labels
        accountLabels = labels
        restoringSelection = true
        tableView.reloadData()
        restoreSelection()
        restoringSelection = false
    }

    required init?(coder: NSCoder) {
        fatalError("AppKitPrototypeList is built in code")
    }

    /// One `.plain`-style column, header hidden, fixed `rowHeight` of
    /// `pitch` minus the intercell spacing so consecutive rows' tops
    /// sit exactly `pitch` apart; native selection, multiple allowed.
    private func buildTable() {
        let column = NSTableColumn(
            identifier: NSUserInterfaceItemIdentifier("messages")
        )
        column.resizingMask = .autoresizingMask
        tableView.addTableColumn(column)
        tableView.headerView = nil
        tableView.style = .plain
        tableView.usesAutomaticRowHeights = false
        tableView.rowHeight =
            MessageRowMetrics.pitch - tableView.intercellSpacing.height
        tableView.allowsMultipleSelection = true
        tableView.columnAutoresizingStyle = .lastColumnOnlyAutoresizingStyle
        tableView.dataSource = self
        tableView.delegate = self
        tableView.menu = contextMenu()
        tableView.setDraggingSourceOperationMask(.move, forLocal: true)
        tableView.setDraggingSourceOperationMask([], forLocal: false)
        tableView.resumeSelection = { [weak self] step in self?.resumeSelection(step) == true }
        tableView.perform = { [weak self] command in self?.perform(command) == true }
        tableView.target = self
        tableView.doubleAction = #selector(openClicked)
        tableView.setAccessibilityIdentifier(Self.accessibilityID)

        scrollView.documentView = tableView
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.autoresizingMask = [.width, .height]
        scrollView.frame = bounds
        addSubview(scrollView)
    }

    /// Scroll the clip view → `setVisible`: the list's only fetch
    /// driver (spec/08).
    private func observeClipView() {
        let clipView = scrollView.contentView
        clipView.postsBoundsChangedNotifications = true
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(clipViewDidChange(_:)),
            name: NSView.boundsDidChangeNotification,
            object: clipView
        )
        clipView.postsFrameChangedNotifications = true
        NotificationCenter.default.addObserver(
            self,
            selector: #selector(clipViewDidChange(_:)),
            name: NSView.frameDidChangeNotification,
            object: clipView
        )
    }

    @objc
    private func clipViewDidChange(_ notification: Notification) {
        pushVisibleRange()
    }

    /// The visible ranks into the model — the sole driver of reads.
    private func pushVisibleRange() {
        model.setVisible(visibleRanks)
    }

    /// Reload the delivered pages' rows. The table touches only rows
    /// that have views — the visible ones and those it prepared just
    /// off screen for responsive scrolling, which would otherwise keep
    /// their placeholders when they scroll in.
    private func pagesDidLand(_ pages: [Int]) {
        var indexes = IndexSet()
        for page in pages {
            let start = page * ListWindowModel.pageSize
            let end = min(start + ListWindowModel.pageSize, model.count)
            guard start < end else { continue }
            indexes.insert(integersIn: start ..< end)
        }
        guard !indexes.isEmpty else { return }
        tableView.reloadData(
            forRowIndexes: indexes,
            columnIndexes: IndexSet(integer: 0)
        )
    }

    /// The id index was replaced (open, a fill's rebuild, a
    /// `resetRequired` rebuild): every row changed, so reload fully
    /// and map the selection back onto the new ranks.
    private func indexDidChange() {
        restoringSelection = true
        defer { restoringSelection = false }
        // Range selection waits for the exact index (spec/08): the
        // provisional prefix isn't the whole window.
        tableView.allowsMultipleSelection = model.hasExactIndex
        tableView.reloadData()
        restoreSelection()
        pushVisibleRange()
    }

    /// Reapply the id-held selection as row indexes — ids that left
    /// the window drop out silently.
    private func restoreSelection() {
        let restoring = restoringSelection
        restoringSelection = true
        defer { restoringSelection = restoring }
        var indexes = IndexSet()
        for id in selectedOccurrenceIDs {
            if let rank = model.index(of: id) {
                indexes.insert(rank)
            }
        }
        if let first = indexes.first { selectionRank = first }
        tableView.selectRowIndexes(indexes, byExtendingSelection: false)
    }

    /// An arrow after the whole selection left: Down takes the row now
    /// at the first one's rank, Up the row before it.
    private func resumeSelection(_ step: MessageTableView.Step) -> Bool {
        guard let resumeRank, model.count > 0 else { return false }
        let wanted = step == .next ? resumeRank : resumeRank - 1
        let row = min(max(wanted, 0), model.count - 1)
        tableView.selectRowIndexes([row], byExtendingSelection: false)
        tableView.scrollRowToVisible(row)
        return true
    }

    /// Return opens the one selected row; with none or several it is
    /// left to AppKit. Space pages whenever paging is wired.
    private func perform(_ command: MessageTableView.Command) -> Bool {
        switch command {
        case .open:
            guard let onOpen, selectedOccurrenceIDs.count == 1, let id = selectedOccurrenceIDs.first else {
                return false
            }
            onOpen(id)
            return true
        case .page(let direction):
            guard let onPage else { return false }
            onPage(direction)
            return true
        }
    }

    /// A double-click opens the row it hit, whatever else is selected.
    @objc private func openClicked() {
        let row = tableView.clickedRow
        guard row >= 0, let id = model.occurrenceID(at: row) else { return }
        onOpen?(id)
    }

    /// Give the table the keyboard, as the filing palette does when it
    /// closes (spec/08 "Keyboard filing").
    func focus() {
        window?.makeFirstResponder(tableView)
    }

    /// The context menu — rebuilt per click in `menuNeedsUpdate`,
    /// when `clickedRow` is valid.
    private func contextMenu() -> NSMenu {
        let menu = NSMenu()
        menu.delegate = self
        return menu
    }

    /// The context menu's action items are no-ops (plan M0.10): the
    /// disabled header already carries what the owner checks — the
    /// target count.
    @objc
    private func contextActionNoOp(_ sender: NSMenuItem) {}
}

// MARK: - Table data source and delegate

extension AppKitPrototypeList: NSTableViewDataSource, NSTableViewDelegate {
    /// Row count straight from the id index.
    func numberOfRows(in tableView: NSTableView) -> Int {
        model.count
    }

    /// A reused cell, configured with the cached row — or the
    /// placeholder while its page is out (spec/08).
    func tableView(
        _ tableView: NSTableView,
        viewFor tableColumn: NSTableColumn?,
        row: Int
    ) -> NSView? {
        let cell =
            tableView.makeView(withIdentifier: Self.cellIdentifier, owner: nil)
            as? MessageRowCellView ?? makeCell()
        let row = model.row(at: row)
        let account = row.flatMap { row in appearanceModel?.appearance?.accounts.first { $0.accountId == row.accountId } }
        cell.configure(content: row.map { formatter.present($0) }, account: account,
                       accountLabel: row.flatMap { accountLabels[$0.accountId] },
                       usesAppearance: appearanceModel != nil)
        return cell
    }

    func tableView(_ tableView: NSTableView, pasteboardWriterForRow row: Int) -> (any NSPasteboardWriting)? {
        guard contextActions != nil, model.occurrenceID(at: row) != nil else { return nil }
        return MessageListDrag.item()
    }

    /// The trackpad swipe commands for one row (spec/08 "Selection"):
    /// trailing files the row, leading flags it. Titles, admission and
    /// the click target come from the same `contextActions` as the
    /// context menu, so a disabled command is never offered and a
    /// selected row acts on the whole selection.
    func tableView(
        _ tableView: NSTableView, rowActionsForRow row: Int, edge: NSTableView.RowActionEdge
    ) -> [NSTableViewRowAction] {
        guard let clickedID = model.occurrenceID(at: row), let contextActions else { return [] }
        let targets = ContextClickTarget.targets(
            clickedID: clickedID, selection: selectedOccurrenceIDs
        )
        let wanted: Set<NSUserInterfaceItemIdentifier> = edge == .trailing
            ? [MainMenuBuilder.ItemID.archive, MainMenuBuilder.ItemID.trash]
            : [MainMenuBuilder.ItemID.toggleRead, MainMenuBuilder.ItemID.toggleStar]
        var actions: [NSTableViewRowAction] = []
        for action in contextActions(targets) where wanted.contains(action.identifier) {
            guard action.enabled else { continue }
            let style: NSTableViewRowAction.Style =
                action.identifier == MainMenuBuilder.ItemID.trash ? .destructive : .regular
            actions.append(
                NSTableViewRowAction(style: style, title: action.title) { _, _ in
                    action.run()
                }
            )
        }
        return actions
    }

    /// The rows are captured as the drag begins: the list may change
    /// under it.
    func tableView(
        _ tableView: NSTableView, draggingSession session: NSDraggingSession,
        willBeginAt screenPoint: NSPoint, forRowIndexes rowIndexes: IndexSet
    ) {
        draggedIDs = Set(rowIndexes.compactMap { model.occurrenceID(at: $0) })
    }

    /// Whether `source` is this list's table.
    func isDragSource(_ source: Any?) -> Bool {
        (source as? NSTableView) === tableView
    }

    private func makeCell() -> MessageRowCellView {
        let cell = MessageRowCellView(showsAccountMarker: accountMarker == .shown)
        cell.identifier = Self.cellIdentifier
        return cell
    }

    /// A row view of our own, so a row answers the pointer the way
    /// Mail's message list does: a subtle fill on hover.
    ///
    /// A fresh one per row: `makeView(withIdentifier:owner:)` dequeues
    /// *cell* views, and asking it for a row view always misses, so the
    /// identifier would be dead code. Row views are pooled by the table
    /// itself, and a recycling pool here would be a second one.
    func tableView(_ tableView: NSTableView, rowViewForRow row: Int) -> NSTableRowView? {
        HoverRowView()
    }

    /// Keep the id-held selection in sync with the table's indexes —
    /// through `occurrenceID`, so no row is ever fetched for it — and
    /// hand it to the model, which keeps it across new indexes.
    func tableViewSelectionDidChange(_ notification: Notification) {
        guard !restoringSelection else { return }
        let ids = Set(
            tableView.selectedRowIndexes.compactMap {
                model.occurrenceID(at: $0)
            }
        )
        selectionRank = tableView.selectedRowIndexes.first
        resumeRank = nil
        guard ids != selectedOccurrenceIDs else { return }
        selectedOccurrenceIDs = ids
        model.select(ids)
        onSelectionChange?(ids)
    }
}

// MARK: - Edit → Undo

/// A window answers `undo:` with its own undo manager, whatever its
/// first responder's is, so the list answers it while it has the
/// keyboard (spec/08 "Keyboard filing").
extension AppKitPrototypeList: NSMenuItemValidation {
    @objc func undo(_ sender: Any?) {
        undoManager?.undo()
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        guard menuItem.action == #selector(undo(_:)) else { return true }
        guard let undoManager else { return false }
        menuItem.title = undoManager.undoMenuItemTitle
        return undoManager.canUndo
    }
}

// MARK: - Context menu

extension AppKitPrototypeList: NSMenuDelegate {
    /// Build the menu for this click. `clickedRow` is valid only
    /// inside the context-menu event, so items are made here: a
    /// disabled header carrying the target count, then the window's
    /// commands, or the prototype's no-ops. A click outside any row
    /// shows no menu.
    func menuNeedsUpdate(_ menu: NSMenu) {
        menu.removeAllItems()
        let row = tableView.clickedRow
        guard row >= 0, let clickedID = model.occurrenceID(at: row)
        else {
            return
        }
        // spec/08 "Selection": a click on a selected row targets the
        // selection; on an unselected row, that row alone — and the
        // selection is left as it was (the default table behavior).
        let targets = ContextClickTarget.targets(
            clickedID: clickedID,
            selection: selectedOccurrenceIDs
        )
        let header = NSMenuItem(
            title: ContextClickTarget.targetSummary(count: targets.count),
            action: nil,
            keyEquivalent: ""
        )
        header.isEnabled = false
        menu.addItem(header)
        menu.addItem(.separator())
        if let contextActions {
            menu.autoenablesItems = false
            for action in contextActions(targets) {
                menu.addItem(MessageContextItem(action))
            }
            return
        }
        for title in ContextClickTarget.actionTitles {
            let item = NSMenuItem(
                title: title,
                action: #selector(contextActionNoOp(_:)),
                keyEquivalent: ""
            )
            item.target = self
            menu.addItem(item)
        }
    }
}

// MARK: - Benchmark seam

extension AppKitPrototypeList: PrototypeList {
    /// `rank` to the viewport's top: scroll the clip view to the row's
    /// top edge and reflect it on the scrollers.
    func scrollToRank(_ rank: Int) {
        guard model.count > 0 else { return }
        let clamped = min(max(0, rank), model.count - 1)
        let top = tableView.rect(ofRow: clamped).minY
        let clipView = scrollView.contentView
        clipView.scroll(to: NSPoint(x: 0, y: top))
        scrollView.reflectScrolledClipView(clipView)
        pushVisibleRange()
    }

    /// The rows the clip view shows — from the scroll position, not
    /// the model, through the same math as the SwiftUI list (rows sit
    /// `pitch` apart from the top). Unlike `rows(in: visibleRect)` it
    /// doesn't wait for the table to retile after a reload.
    var visibleRanks: Range<Int> {
        let clip = scrollView.contentView.bounds
        return MessageRowMetrics.visibleRanks(
            top: clip.minY,
            height: clip.height,
            count: model.count
        )
    }

    /// `rank`'s rect converted into window coordinates for the
    /// harness's synthetic clicks.
    func rowRect(forRank rank: Int) -> NSRect? {
        guard rank >= 0, rank < model.count else { return nil }
        return tableView.convert(tableView.rect(ofRow: rank), to: nil)
    }

    var selectedIDs: Set<Int64> {
        selectedOccurrenceIDs
    }

    var listView: NSView {
        scrollView
    }
}

// MARK: - The row cell

/// The reused cell. Every subview is created once and framed by hand
/// in `layoutSubviews()` — no Auto Layout in cells (plan M0.10).
/// `isFlipped` keeps the layout math top-down like the SwiftUI row's.
final class MessageRowCellView: NSTableCellView {
    var contrastOptions: () -> (increaseContrast: Bool, differentiate: Bool) = {
        (NSWorkspace.shared.accessibilityDisplayShouldIncreaseContrast,
         NSWorkspace.shared.accessibilityDisplayShouldDifferentiateWithoutColor)
    }
    /// The trailing date's reserved width: measured from the formatted
    /// string, clamped so a time-only date does not waste the text
    /// column while a long date never takes more than before.
    static func dateColumnWidth(for date: String, font: NSFont) -> CGFloat {
        let text = (date as NSString).size(withAttributes: [.font: font])
        return min(84, max(32, ceil(text.width) + 2))
    }

    /// This row's reserved date width, set from its formatted date.
    private var reservedDateWidth: CGFloat = 84

    private let rail = NSView()
    private let railMarker = NSView()
    private let railLeftEdge = NSView()
    private let railRightEdge = NSView()
    private let monogram = BadgeView()
    private let unreadDot = BadgeView()
    private let senderField = NSTextField(labelWithString: "")
    private let dateField = NSTextField(labelWithString: "")
    private let accountField = NSTextField(labelWithString: "")
    private let subjectField = NSTextField(labelWithString: "")
    private let snippetField = NSTextField(labelWithString: "")
    private let flagMarker = NSImageView()
    private let attachmentMarker = NSImageView()
    private let tagChips = (0 ..< MessageRowMetrics.visibleTagCount).map {
        _ in BadgeView()
    }
    /// Draws `+N` when a row carries more tags than the chips show.
    private let overflowChip = BadgeView()
    /// The current row's collapsed-tag count, so layout can decide the
    /// chip's visibility afresh on every pass (a window that widens
    /// brings it back; one that narrows drops it).
    private var overflowCount = 0
    private let placeholderFill = NSView()

    /// The row's account color, kept so the layer colors can be
    /// resolved again when the appearance changes.
    private var accountColor: NSColor = .clear
    private var accountAppearance: AccountAppearance?

    /// Whether the rail and monogram show; without them the text
    /// starts at the leading edge.
    private let showsAccountMarker: Bool

    /// Whether the rail and the monogram are drawn, as `configure`
    /// last decided. Read by tests, which otherwise have to walk
    /// `subviews` by an index nothing guarantees.
    private(set) var showsMarker = false

    /// Everything but the placeholder — hidden together when the row
    /// is out.
    private var contentSubviews: [NSView] {
        [rail, monogram, unreadDot, senderField, dateField, accountField, subjectField,
         snippetField, flagMarker, attachmentMarker] + tagChips + [overflowChip]
    }

    init(showsAccountMarker: Bool) {
        self.showsAccountMarker = showsAccountMarker
        super.init(frame: .zero)
        NSWorkspace.shared.notificationCenter.addObserver(
            self, selector: #selector(contrastDidChange),
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil
        )

        rail.wantsLayer = true
        for view in [railMarker, railLeftEdge, railRightEdge] {
            view.wantsLayer = true
            rail.addSubview(view)
        }
        addSubview(rail)

        monogram.titleColor = .white
        monogram.font = .systemFont(ofSize: 11, weight: .semibold)
        addSubview(monogram)

        unreadDot.fillColor = .controlAccentColor
        addSubview(unreadDot)

        for field in [senderField, subjectField, snippetField, dateField, accountField] {
            field.lineBreakMode = .byTruncatingTail
            field.maximumNumberOfLines = 1
            addSubview(field)
        }
        subjectField.font = .systemFont(ofSize: 13)
        snippetField.font = .systemFont(ofSize: 12)
        snippetField.textColor = .secondaryLabelColor
        dateField.font = .systemFont(ofSize: 12)
        dateField.textColor = .secondaryLabelColor
        dateField.alignment = .right
        accountField.font = .systemFont(ofSize: 11, weight: .semibold)
        accountField.alignment = .right
        accountField.lineBreakMode = .byTruncatingTail

        // Template + tint: one code path that works for every marker,
        // no symbol-configuration version dance.
        flagMarker.image = NSImage(
            systemSymbolName: "flag.fill", accessibilityDescription: nil
        )
        flagMarker.image?.isTemplate = true
        flagMarker.contentTintColor = .systemOrange
        flagMarker.imageScaling = .scaleProportionallyDown
        addSubview(flagMarker)
        attachmentMarker.image = NSImage(
            systemSymbolName: "paperclip", accessibilityDescription: nil
        )
        attachmentMarker.image?.isTemplate = true
        attachmentMarker.contentTintColor = .secondaryLabelColor
        attachmentMarker.imageScaling = .scaleProportionallyDown
        addSubview(attachmentMarker)

        for chip in tagChips {
            chip.fillColor = .quaternaryLabelColor
            chip.titleColor = .secondaryLabelColor
            chip.font = .systemFont(ofSize: 10)
            addSubview(chip)
        }
        overflowChip.fillColor = .quaternaryLabelColor
        overflowChip.titleColor = .secondaryLabelColor
        overflowChip.font = .systemFont(ofSize: 10)
        addSubview(overflowChip)

        placeholderFill.wantsLayer = true
        placeholderFill.layer?.cornerRadius =
            MessageRowMetrics.placeholderCornerRadius
        placeholderFill.isHidden = true
        addSubview(placeholderFill)

        // VoiceOver reads the cell's one label, not its parts.
        for subview in contentSubviews + [placeholderFill] {
            subview.setAccessibilityElement(false)
        }
        applyLayerColors()
    }

    required init?(coder: NSCoder) {
        fatalError("MessageRowCellView is built in code")
    }

    @objc private func contrastDidChange() { applyLayerColors() }

    /// Layout math runs in `bounds`, top-first.
    override var isFlipped: Bool { true }

    /// Fill the cell from the shared content; a nil row shows the
    /// placeholder — same height, no text, a subtle fill (spec/08).
    /// The placeholder is no accessibility element: an unlabeled row
    /// is worse than an absent one.
    func configure(content: MessageRowContent?, account: AccountAppearance? = nil,
                   accountLabel: String? = nil, usesAppearance: Bool = false) {
        placeholderFill.isHidden = content != nil
        for subview in contentSubviews {
            subview.isHidden = content == nil
        }
        // A reused cell must not keep a marker hidden from a row whose
        // account had no appearance yet: the subscription that fills
        // `AppearanceModel.appearance` answers after the list has
        // opened, so the first cells a window makes are configured
        // without one and a later reuse has to bring the marker back.
        showsMarker = showsAccountMarker && (!usesAppearance || account != nil)
        rail.isHidden = !showsMarker
        monogram.isHidden = !showsMarker
        guard let content else {
            overflowCount = 0
            setAccessibilityElement(false)
            setAccessibilityLabel(nil)
            return
        }

        accountAppearance = account
        accountColor = AppKitPrototypeList.accountColor(
            at: content.accountColorIndex
        )
        applyLayerColors()
        monogram.title = account?.monogram ?? content.monogram
        accountField.stringValue = accountLabel ?? ""
        accountField.isHidden = !showsAccountMarker || account?.collides != true

        senderField.stringValue = content.sender
        senderField.font = content.unread
            ? .boldSystemFont(ofSize: 13) : .systemFont(ofSize: 13)
        dateField.stringValue = content.date
        reservedDateWidth = Self.dateColumnWidth(
            for: content.date, font: dateField.font ?? .systemFont(ofSize: 12))
        subjectField.stringValue = content.subject
        snippetField.stringValue = content.snippet

        unreadDot.isHidden = !content.unread
        flagMarker.isHidden = !content.flagged
        attachmentMarker.isHidden = !content.hasAttachments
        assert(
            tagChips.count == MessageRowMetrics.visibleTagCount,
            "Add or remove tag chip views to match MessageRowMetrics.visibleTagCount"
        )
        let visibleTags = content.visibleTags
        for (index, chip) in tagChips.enumerated() {
            if index < visibleTags.count {
                chip.title = visibleTags[index]
                chip.isHidden = false
            } else {
                chip.isHidden = true
            }
        }
        overflowCount = content.hiddenTagCount
        overflowChip.title = overflowCount > 0 ? "+\(overflowCount)" : ""
        overflowChip.isHidden = overflowCount == 0

        setAccessibilityElement(true)
        let label = showsAccountMarker && account != nil ? ", account \(accountLabel ?? "")" : ""
        setAccessibilityLabel(content.accessibilityLabel + content.accessibilityOverflowSuffix + label)
        needsLayout = true
    }

    /// Layer colors are `CGColor`s, fixed when set: resolve them again
    /// when the appearance changes, so Dark Mode switches cleanly.
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        applyLayerColors()
    }

    /// The rail and the placeholder fill, resolved for this cell's
    /// appearance.
    private func applyLayerColors() {
        effectiveAppearance.performAsCurrentDrawingAppearance {
            let dark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            let variant = accountAppearance.map { dark ? $0.tokens.dark : $0.tokens.light }
            let marker = variant.map { appearanceColor($0.markerSrgb) } ?? accountColor
            rail.layer?.backgroundColor = (variant.map { appearanceColor($0.stripSrgb) } ?? accountColor).cgColor
            railMarker.layer?.backgroundColor = marker.cgColor
            let outline = variant.map { appearanceColor($0.outlineSrgb) } ?? accountColor
            railLeftEdge.layer?.backgroundColor = outline.cgColor
            railRightEdge.layer?.backgroundColor = outline.cgColor
            monogram.fillColor = variant.map { appearanceColor($0.stripSrgb) } ?? accountColor
            monogram.titleColor = variant.map { appearanceColor($0.markerSrgb) } ?? .white
            let name = effectiveAppearance.name
            let options = contrastOptions()
            let strong = name == .accessibilityHighContrastAqua || name == .accessibilityHighContrastDarkAqua
                || options.increaseContrast || options.differentiate
            monogram.outlineColor = strong ? outline : nil
            monogram.font = .systemFont(ofSize: 11, weight: strong ? .bold : .semibold)
            placeholderFill.layer?.backgroundColor = NSColor.labelColor
                .withAlphaComponent(MessageRowMetrics.placeholderFillOpacity)
                .cgColor
        }
    }

    override func layout() {
        super.layout()
        layoutSubviews()
    }

    /// `layout()` isn't guaranteed on a plain resize without the
    /// constraint engine, so frame changes get their own pass.
    override func resizeSubviews(withOldSize oldSize: NSSize) {
        super.resizeSubviews(withOldSize: oldSize)
        layoutSubviews()
    }

    /// Frame every subview by hand — the row is `pitch` tall in three
    /// text zones: sender + markers + date, subject, snippet + chips.
    private func layoutSubviews() {
        let width = bounds.width
        let metrics = MessageRowMetrics.self

        placeholderFill.frame = bounds.insetBy(
            dx: metrics.placeholderInsetX,
            dy: metrics.placeholderInsetY
        )

        rail.frame = NSRect(
            x: 0, y: 0,
            width: metrics.railSurfaceWidth, height: bounds.height
        )
        railMarker.frame = NSRect(x: 3, y: 0, width: metrics.railWidth, height: bounds.height)
        railLeftEdge.frame = NSRect(x: 0, y: 0, width: 1, height: bounds.height)
        railRightEdge.frame = NSRect(x: metrics.railSurfaceWidth - 1, y: 0, width: 1, height: bounds.height)
        monogram.frame = NSRect(
            x: metrics.railSurfaceWidth + metrics.innerGap,
            y: (bounds.height - metrics.monogramSize) / 2,
            width: metrics.monogramSize, height: metrics.monogramSize
        )
        let contentX = showsAccountMarker
            ? rail.frame.maxX + metrics.innerGap + metrics.monogramSize + metrics.innerGap
            : metrics.innerGap

        // Top line: [dot] sender … [flag] [paperclip] date.
        var trailing = width - metrics.trailingPadding
        dateField.frame = NSRect(
            x: trailing - reservedDateWidth, y: 7,
            width: reservedDateWidth, height: 16
        )
        trailing = dateField.frame.minX - metrics.markerSpacing
        if !accountField.isHidden {
            let labelWidth = min(100, max(0, trailing - contentX), accountField.intrinsicContentSize.width)
            accountField.frame = NSRect(x: trailing - labelWidth, y: 7, width: labelWidth, height: 16)
            trailing = accountField.frame.minX - metrics.markerSpacing
        }
        // Right-to-left: the paperclip lands nearest the date, the
        // flag left of it — visually [flag] [paperclip] date.
        for marker in [attachmentMarker, flagMarker] where !marker.isHidden {
            marker.frame = NSRect(
                x: trailing - metrics.markerSize, y: 8.5,
                width: metrics.markerSize, height: metrics.markerSize
            )
            trailing = marker.frame.minX - metrics.markerSpacing
        }

        var leading = contentX
        if !unreadDot.isHidden {
            unreadDot.frame = NSRect(
                x: leading, y: 11.5,
                width: metrics.unreadDotSize, height: metrics.unreadDotSize
            )
            leading += metrics.unreadDotSize + metrics.markerSpacing
        }
        senderField.frame = NSRect(
            x: leading, y: 7,
            width: max(0, trailing - leading), height: 16
        )

        // Subject, one line.
        subjectField.frame = NSRect(
            x: contentX, y: 26,
            width: max(0, width - contentX - metrics.trailingPadding),
            height: 15
        )

        // Snippet with right-aligned tag chips. The chips stop at
        // `contentX`: without a floor, three long labels walk past the
        // text column, draw over the rail and outside the cell, and
        // squeeze the snippet to nothing. A chip that no longer fits is
        // hidden rather than clipped, and one that fits again after the
        // row widens is brought back.
        //
        // The pass only ever hides: `configure` has already hidden the
        // chips past this row's visible tags, so a pooled cell cannot
        // resurrect a chip that belongs to another message. Which of the
        // visible chips fit is therefore whatever packs from the right,
        // which is not a contiguous run — a wide tag can drop out while
        // a narrower one beside it stays.
        let chipFloor = contentX
        var chipTrail = width - metrics.trailingPadding
        for chip in tagChips.reversed() where !chip.isHidden {
            let chipWidth = chip.fittingWidth()
            guard chipTrail - chipWidth >= chipFloor else {
                chip.isHidden = true
                continue
            }
            chip.frame = NSRect(
                x: chipTrail - chipWidth, y: 40.5,
                width: chipWidth, height: 14
            )
            chipTrail = chip.frame.minX - metrics.markerSpacing
        }
        // Layout owns the chip's visibility: hide it when there is no
        // overflow, show it only while it fits beside the visible tags.
        overflowChip.isHidden = true
        if overflowCount > 0 {
            let chipWidth = overflowChip.fittingWidth()
            if chipTrail - chipWidth >= contentX {
                overflowChip.frame = NSRect(
                    x: chipTrail - chipWidth, y: 40.5,
                    width: chipWidth, height: 14
                )
                overflowChip.isHidden = false
                chipTrail = overflowChip.frame.minX - metrics.markerSpacing
            } else {
                // Too narrow for the chip beside the visible tags:
                // show the tags alone rather than overlap them.
                overflowChip.isHidden = true
            }
        }
        snippetField.frame = NSRect(
            x: contentX, y: 41,
            width: max(0, chipTrail - contentX - metrics.markerSpacing),
            height: 13
        )
    }
}

extension AppKitPrototypeList {
    /// The palette slot's color.
    static func accountColor(at index: Int) -> NSColor {
        accountPalette[index % accountPalette.count]
    }
}

// MARK: - Row view

/// The list's row view: a subtle fill while the pointer is over the
/// row, the way Mail's message list answers a hover. The fill sits
/// under the cell the table adds afterwards. The selection highlight
/// is painted by the row itself, beneath the fill, so `hoverAlpha`
/// fades the fill out while selected instead of stacking two signals.
@MainActor
final class HoverRowView: NSTableRowView {
    /// The hover fill. Insets keep it off the row's edges, so the
    /// rail and the list's separator stay readable.
    private let hoverFill = NSView()
    private var trackingArea: NSTrackingArea?
    private var hovering = false

    /// The fill's corner radius: a rounded rectangle of its own, not
    /// the placeholder's — the two visuals are unrelated.
    private static let cornerRadius: CGFloat = 6

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        hoverFill.wantsLayer = true
        hoverFill.layer?.cornerRadius = Self.cornerRadius
        hoverFill.alphaValue = 0
        addSubview(hoverFill)
    }

    required init?(coder: NSCoder) {
        fatalError("HoverRowView is built in code")
    }

    /// Resolve the fill for the appearance it is in, so a Dark Mode
    /// switch and Increase Contrast both take effect without a reload.
    /// The color is resolved inside the appearance: `cgColor` on a
    /// dynamic color otherwise resolves against whatever appearance
    /// happens to be current rather than this view's.
    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        // A 5 % tint is faint by design, and invisible once Increase
        // Contrast has taken the rest of the window's subtlety away.
        let alpha = Self.fillAlpha(
            increaseContrast: NSWorkspace.shared.accessibilityDisplayShouldIncreaseContrast
        )
        var fill: CGColor?
        effectiveAppearance.performAsCurrentDrawingAppearance {
            fill = NSColor.labelColor.withAlphaComponent(alpha).cgColor
        }
        hoverFill.layer?.backgroundColor = fill
    }

    override func layout() {
        super.layout()
        // One point of inset vertically keeps the fill inside the row
        // pitch, so it never bleeds into the gap between rows.
        hoverFill.frame = bounds.insetBy(dx: 3, dy: 1)
    }

    /// The fill is decoration: a hit that lands on it falls through to
    /// the row, so the gutter still selects the row the way the default
    /// row view did.
    override func hitTest(_ point: NSPoint) -> NSView? {
        let hit = super.hitTest(point)
        return hit === hoverFill ? nil : hit
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let trackingArea {
            removeTrackingArea(trackingArea)
        }
        // `bounds`, not `.zero`: `.inVisibleRect` keeps the area in
        // the view's visible rectangle, and whether AppKit substitutes
        // or intersects it with the rect passed here, `bounds` is right
        // either way.
        let area = NSTrackingArea(
            rect: bounds,
            options: [.mouseEnteredAndExited, .activeInActiveApp, .inVisibleRect],
            owner: self,
            userInfo: nil
        )
        addTrackingArea(area)
        trackingArea = area
        // A row can appear under a stationary pointer — a reload, or a
        // row scrolled in beneath the cursor. AppKit sends no
        // `mouseEntered` until the pointer moves, so the state is
        // primed here or the highlight pops in a beat late.
        // `mouseLocationOutsideOfEventStream` is in screen coordinates,
        // so it becomes window coordinates before this view's own
        // conversion, or the hit test is offset by the window's origin.
        hovering = window.map {
            // `convertFromScreen` is the rect form on `NSWindow`, so the
            // screen point travels as a zero-size rect and comes back as
            // its origin.
            let inWindow = $0.convertFromScreen(
                NSRect(origin: $0.mouseLocationOutsideOfEventStream, size: .zero)
            ).origin
            let here: NSPoint = convert(inWindow, from: nil)
            return bounds.contains(here)
        } ?? false
        // Applied rather than animated: a row that appears under a
        // stationary cursor should be filled at once, and this must not
        // depend on AppKit posting a `mouseEntered` for an area that
        // was installed under a pointer that is not moving.
        // A fade still in flight finishes at its own target and would
        // re-assert it, so it is cancelled first.
        hoverFill.layer?.removeAllAnimations()
        hoverFill.alphaValue = Self.hoverAlpha(hovering: hovering, selected: isSelected)
    }

    override func mouseEntered(with event: NSEvent) {
        hovering = true
        animateHover()
    }

    override func mouseExited(with event: NSEvent) {
        hovering = false
        animateHover()
    }

    /// The selection hides the hover fill: two fills on one row read
    /// as a bug, and AppKit's highlight is the stronger signal.
    override var isSelected: Bool {
        didSet {
            if oldValue != isSelected { animateHover() }
        }
    }

    private func animateHover() {
        NSAnimationContext.runAnimationGroup { context in
            context.duration = 0.12
            context.timingFunction = CAMediaTimingFunction(name: .easeOut)
            hoverFill.animator().alphaValue = Self.hoverAlpha(hovering: hovering, selected: isSelected)
        }
    }

    /// The hover fill's target alpha. Pure, so the rule is testable
    /// without driving the pointer.
    nonisolated static func hoverAlpha(hovering: Bool, selected: Bool) -> CGFloat {
        hovering && !selected ? 1 : 0
    }

    /// The fill's alpha: faint by design, and stronger once Increase
    /// Contrast has taken the rest of the window's subtlety away.
    nonisolated static func fillAlpha(increaseContrast: Bool) -> CGFloat {
        increaseContrast ? 0.14 : 0.05
    }
}

// MARK: - Badges

/// A drawn capsule with centered text — one shape serving the unread
/// dot (empty title), the account monogram and the tag chips.
private final class BadgeView: NSView {
    var outlineColor: NSColor? { didSet { needsDisplay = true } }
    var title = "" {
        didSet { needsDisplay = true }
    }
    /// Match the cell's top-down coordinates so the centered-title
    /// math means the same thing in both.
    override var isFlipped: Bool { true }
    var fillColor: NSColor = .quaternaryLabelColor {
        didSet { needsDisplay = true }
    }
    var titleColor: NSColor = .secondaryLabelColor {
        didSet { needsDisplay = true }
    }
    var font = NSFont.systemFont(ofSize: 10) {
        didSet { needsDisplay = true }
    }

    /// The width the capsule needs for its title plus padding.
    func fittingWidth() -> CGFloat {
        let size = (title as NSString).size(
            withAttributes: [.font: font]
        )
        return ceil(size.width) + 2 * MessageRowMetrics.tagChipPadding
    }

    override func draw(_ dirtyRect: NSRect) {
        fillColor.setFill()
        let shape = NSBezierPath(
            roundedRect: bounds,
            xRadius: bounds.height / 2,
            yRadius: bounds.height / 2
        )
        shape.fill()
        if let outlineColor {
            outlineColor.setStroke()
            shape.lineWidth = 2
            shape.stroke()
        }
        guard !title.isEmpty else { return }
        let attributes: [NSAttributedString.Key: Any] = [
            .font: font,
            .foregroundColor: titleColor,
        ]
        let size = (title as NSString).size(withAttributes: attributes)
        (title as NSString).draw(
            at: NSPoint(
                x: bounds.midX - size.width / 2,
                y: bounds.midY - size.height / 2
            ),
            withAttributes: attributes
        )
    }
}
