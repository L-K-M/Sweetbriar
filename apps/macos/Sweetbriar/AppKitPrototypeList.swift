// The AppKit list (plan M0.10, the production list since M1.4): a
// view-based `NSTableView` of fixed-height cells with no Auto Layout
// inside them, driven by the bridge's `ListWindowModel`. Pages land
// through `onPagesDelivered` and reload only the delivered pages'
// visible rows; the clip view's bounds changes drive `setVisible`.
// Selection is held by occurrence id, in the model too, so a 10k-row
// selection fetches nothing and follows its messages across indexes.

import AppKit
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
        /// Until M1.10's appearance service gives accounts colors:
        /// none is derived from an id (spec/08 "Sidebar").
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
    private let selectionLoss: SelectionLoss
    private let scrollView = NSScrollView()
    private let tableView = NSTableView()

    /// The selection, by occurrence id, whenever it changes — the
    /// list's own clicks and a new index's changes alike.
    var onSelectionChange: (@MainActor (Set<Int64>) -> Void)?

    /// True while the table's selection is set from the ids, not by the
    /// user: its callbacks then must not overwrite the model's ids — a
    /// new index may lack one the model is still following (spec/08).
    private var restoringSelection = false

    /// The selection as occurrence ids — the source of truth the
    /// table's `selectedRowIndexes` is mapped through.
    private var selectedOccurrenceIDs: Set<Int64> = []

    init(
        model: ListWindowModel,
        formatter: MessageRowFormatter,
        accountMarker: AccountMarker = .shown,
        selectionLoss: SelectionLoss = .keepPosition,
        onIndexChange: (@MainActor () -> Void)? = nil
    ) {
        self.model = model
        self.formatter = formatter
        self.accountMarker = accountMarker
        self.selectionLoss = selectionLoss
        super.init(frame: .zero)
        buildTable()
        observeClipView()

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
            selectedOccurrenceIDs = ids
            restoreSelection()
            if lost, selectionLoss == .reopenAtTop {
                scrollToRank(0)
            }
            onSelectionChange?(ids)
        }
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
        tableView.selectRowIndexes(indexes, byExtendingSelection: false)
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
        cell.configure(content: model.row(at: row).map { formatter.present($0) })
        return cell
    }

    private func makeCell() -> MessageRowCellView {
        let cell = MessageRowCellView(showsAccountMarker: accountMarker == .shown)
        cell.identifier = Self.cellIdentifier
        return cell
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
        guard ids != selectedOccurrenceIDs else { return }
        selectedOccurrenceIDs = ids
        model.select(ids)
        onSelectionChange?(ids)
    }
}

// MARK: - Context menu

extension AppKitPrototypeList: NSMenuDelegate {
    /// Build the menu for this click. `clickedRow` is valid only
    /// inside the context-menu event, so items are made here: a
    /// disabled header carrying the target count, then the four
    /// no-op commands. A click outside any row shows no menu.
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
private final class MessageRowCellView: NSTableCellView {
    /// The trailing date's reserved width — wide enough for the short
    /// date form.
    private static let dateWidth: CGFloat = 84

    /// The tag chip pool: synthetic rows carry at most two; extras
    /// stay hidden.
    private static let tagChipCount = 3

    private let rail = NSView()
    private let monogram = BadgeView()
    private let unreadDot = BadgeView()
    private let senderField = NSTextField(labelWithString: "")
    private let dateField = NSTextField(labelWithString: "")
    private let subjectField = NSTextField(labelWithString: "")
    private let snippetField = NSTextField(labelWithString: "")
    private let flagMarker = NSImageView()
    private let attachmentMarker = NSImageView()
    private let tagChips = (0 ..< MessageRowCellView.tagChipCount).map {
        _ in BadgeView()
    }
    private let placeholderFill = NSView()

    /// The row's account color, kept so the layer colors can be
    /// resolved again when the appearance changes.
    private var accountColor: NSColor = .clear

    /// Whether the rail and monogram show; without them the text
    /// starts at the leading edge.
    private let showsAccountMarker: Bool

    /// Everything but the placeholder — hidden together when the row
    /// is out.
    private var contentSubviews: [NSView] {
        [rail, monogram, unreadDot, senderField, dateField, subjectField,
         snippetField, flagMarker, attachmentMarker] + tagChips
    }

    init(showsAccountMarker: Bool) {
        self.showsAccountMarker = showsAccountMarker
        super.init(frame: .zero)

        rail.wantsLayer = true
        addSubview(rail)

        monogram.titleColor = .white
        monogram.font = .systemFont(ofSize: 11, weight: .semibold)
        addSubview(monogram)

        unreadDot.fillColor = .controlAccentColor
        addSubview(unreadDot)

        for field in [senderField, subjectField, snippetField, dateField] {
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

    /// Layout math runs in `bounds`, top-first.
    override var isFlipped: Bool { true }

    /// Fill the cell from the shared content; a nil row shows the
    /// placeholder — same height, no text, a subtle fill (spec/08).
    /// The placeholder is no accessibility element: an unlabeled row
    /// is worse than an absent one.
    func configure(content: MessageRowContent?) {
        placeholderFill.isHidden = content != nil
        for subview in contentSubviews {
            subview.isHidden = content == nil
        }
        if !showsAccountMarker {
            rail.isHidden = true
            monogram.isHidden = true
        }
        guard let content else {
            setAccessibilityElement(false)
            setAccessibilityLabel(nil)
            return
        }

        accountColor = AppKitPrototypeList.accountColor(
            at: content.accountColorIndex
        )
        applyLayerColors()
        monogram.fillColor = accountColor
        monogram.title = content.monogram

        senderField.stringValue = content.sender
        senderField.font = content.unread
            ? .boldSystemFont(ofSize: 13) : .systemFont(ofSize: 13)
        dateField.stringValue = content.date
        subjectField.stringValue = content.subject
        snippetField.stringValue = content.snippet

        unreadDot.isHidden = !content.unread
        flagMarker.isHidden = !content.flagged
        attachmentMarker.isHidden = !content.hasAttachments
        for (index, chip) in tagChips.enumerated() {
            if index < content.tags.count {
                chip.title = content.tags[index]
                chip.isHidden = false
            } else {
                chip.isHidden = true
            }
        }

        setAccessibilityElement(true)
        setAccessibilityLabel(content.accessibilityLabel)
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
            rail.layer?.backgroundColor = accountColor.cgColor
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
            width: metrics.railWidth, height: bounds.height
        )
        monogram.frame = NSRect(
            x: metrics.railWidth + metrics.innerGap,
            y: (bounds.height - metrics.monogramSize) / 2,
            width: metrics.monogramSize, height: metrics.monogramSize
        )
        let contentX = showsAccountMarker
            ? rail.frame.maxX + metrics.innerGap + metrics.monogramSize + metrics.innerGap
            : metrics.innerGap

        // Top line: [dot] sender … [flag] [paperclip] date.
        var trailing = width - metrics.trailingPadding
        dateField.frame = NSRect(
            x: trailing - Self.dateWidth, y: 7,
            width: Self.dateWidth, height: 16
        )
        trailing = dateField.frame.minX - metrics.markerSpacing
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

        // Snippet with right-aligned tag chips.
        var chipTrail = width - metrics.trailingPadding
        for chip in tagChips.reversed() where !chip.isHidden {
            let chipWidth = chip.fittingWidth()
            chip.frame = NSRect(
                x: chipTrail - chipWidth, y: 40,
                width: chipWidth, height: 15
            )
            chipTrail = chip.frame.minX - metrics.markerSpacing
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

// MARK: - Badges

/// A drawn capsule with centered text — one shape serving the unread
/// dot (empty title), the account monogram and the tag chips.
private final class BadgeView: NSView {
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
        NSBezierPath(
            roundedRect: bounds,
            xRadius: bounds.height / 2,
            yRadius: bounds.height / 2
        ).fill()
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
