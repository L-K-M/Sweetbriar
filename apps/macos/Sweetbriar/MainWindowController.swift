// The main window (spec/08 "Windows"): sidebar | message list |
// reader, as an `NSSplitViewController`. It shows the core's open
// status until the core is up, then the sidebar over `SidebarModel`
// with the Unified Inbox chosen.
//
//   ┌──────────┬──────────────────┬──────────────┐
//   │ Sidebar  │ Message list     │ Reader       │
//   │ (model)  │ (ListWindowModel)│ (reader)     │
//   └────┬─────┴────────▲─────────┴──────▲───────┘
//        │ choose(spec) │   selection    │
//        └──────────────┘ ───────────────┘
//   a list's newer generation ──► the sidebar re-reads its counts
//   Drafts (local) and Out Box put their panes in the list pane
//   instead; the reader then shows nothing (spec/08).

import AppKit
import SwiftUI
import SweetbriarBridge

@MainActor
final class MainWindowController: NSWindowController, NSWindowDelegate {
    static let defaultContentSize = NSSize(width: 1100, height: 700)
    /// The window's title, also how UI tests find it.
    static let title = "Sweetbriar"
    private static let sidebarMinimumWidth: CGFloat = 180
    private static let listMinimumWidth: CGFloat = 320
    private static let toastMargin: CGFloat = 20

    private let session: CoreSession
    private let appearance: SharedAppearance
    private let backups: SharedBackups
    private let readingViewMemory: SharedReadingViewMemory
    private let openBackups: @MainActor () -> Void
    private let openDraft: @MainActor (String) -> Void
    private let writes = ArchiveWriteTasks()
    private(set) var bridge: CoreBridge?
    /// The message list's single selected occurrence, when the list
    /// shows messages and exactly one occurrence is selected; Reply,
    /// Reply All and Forward act on it (spec/08 "Cross-account
    /// check").
    var selectedOccurrenceId: Int64? {
        guard listPane?.pane == .messages,
            let ids = messageList?.list?.selectedIDs,
            ids.count == 1
        else { return nil }
        return ids.first
    }

    /// The app's one Activity model, started with the core and
    /// restarted after a restore; the sidebar and the Activity window
    /// share it.
    private(set) var activity: ActivityModel?
    private(set) var activitySourceID: ObjectIdentifier?
    private var contentTasks = ArchiveViewTasks()
    private var contentSerial: UInt64 = 0
    private var retirement: Task<Void, Never>?
    private let statusLabel = NSTextField(labelWithString: "Opening the archive…")
    private(set) var sidebarModel: SidebarModel?
    private(set) var sidebar: SidebarViewController?
    private(set) var messageList: MessageListViewController?
    private(set) var drafts: DraftsListViewController?
    private(set) var outBox: OutBoxViewController?
    private var listPane: ListPaneController?
    private var imports: ImportCoordinator?
    private var signIn: ProtonSignInSheet?
    private var reader: ReaderViewController?
    /// Marks a shown unread Proton message read (spec/08 "Reader").
    private var markRead: MarkReadTimer?
    private(set) var filing: FilingController?
    private var closed = false


    init(session: CoreSession, appearance: SharedAppearance, backups: SharedBackups,
         readingViewMemory: SharedReadingViewMemory,
         openBackups: @escaping @MainActor () -> Void = {},
         openDraft: @escaping @MainActor (String) -> Void = { _ in }) {
        self.session = session
        self.appearance = appearance
        self.backups = backups
        self.readingViewMemory = readingViewMemory
        self.openBackups = openBackups
        self.openDraft = openDraft
        // Not full-size content: the panes start below the toolbar, and
        // AppKit would blur a title bar's height of them as if they ran
        // under it.
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.defaultContentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = Self.title
        window.isReleasedWhenClosed = false
        let content = NSView(frame: NSRect(origin: .zero, size: Self.defaultContentSize))
        statusLabel.alignment = .center
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(statusLabel)
        NSLayoutConstraint.activate([
            statusLabel.centerXAnchor.constraint(equalTo: content.centerXAnchor),
            statusLabel.centerYAnchor.constraint(equalTo: content.centerYAnchor),
            statusLabel.widthAnchor.constraint(lessThanOrEqualTo: content.widthAnchor, constant: -40),
        ])
        window.contentView = content
        super.init(window: window)
        window.delegate = self
        let toolbar = NSToolbar(identifier: "ch.lkmc.Sweetbriar.mainToolbar")
        toolbar.delegate = self
        toolbar.displayMode = .iconOnly
        window.toolbar = toolbar
        window.center()
    }

    required init?(coder: NSCoder) {
        fatalError("MainWindowController is built in code")
    }

    /// Wait for the core, then build the panes; a failed open stays in
    /// the window.
    func start() {
        contentTasks.run { [weak self] in
            guard let self else { return }
            do {
                let bridge = try await session.bridge()
                try Task.checkCancellation()
                guard backups.model(source: bridge) != nil else { return }
                install(bridge)
            } catch is CancellationError {
            } catch {
                statusLabel.stringValue = "Couldn't open the archive: \(error.localizedDescription)"
            }
        }
    }

    override func showWindow(_ sender: Any?) {
        super.showWindow(sender)
        guard closed else { return }
        closed = false
        if listPane?.pane != .drafts && listPane?.pane != .outBox {
            reader?.select(messageList?.list?.selectedIDs ?? [])
        }
    }

    func windowWillClose(_ notification: Notification) {
        closed = true
        // Dock reopen reuses this controller, so release without retiring its model.
        reader?.select([])
        markRead?.show(nil)
    }

    /// Retire all old-context work before the shell opens any new handles.
    func retireContent() async {
        if let retirement { await retirement.value; return }
        contentSerial &+= 1
        contentTasks.retire()
        // The Activity model's reads and subscription must not outlive
        // this content; rebuildAfterRestore starts it again.
        retireActivity()
        sidebar?.onChoose = nil
        sidebar?.onChooseDrafts = nil
        sidebar?.onChooseOutBox = nil
        let drafts = self.drafts
        drafts?.retire()
        let outBox = self.outBox
        outBox?.retire()
        messageList?.onGeneration = nil
        messageList?.onSelectionChange = nil
        markRead?.cancel()
        filing?.retire()
        filing = nil
        retireWritersForRestore()
        let list = messageList
        let reader = self.reader
        reader?.close()
        sidebarModel = nil
        sidebar = nil
        messageList = nil
        self.drafts = nil
        self.outBox = nil
        listPane = nil
        self.reader = nil
        imports = nil
        signIn = nil
        showArchiveStatus("Updating the archive…")
        let tasks = contentTasks
        let writes = self.writes
        let task = Task {
            await writes.waitForRetirement()
            await list?.close()
            await drafts?.waitForRetirement()
            await outBox?.waitForRetirement()
            await reader?.closeAndWait()
            await tasks.waitForRetirement()
        }
        retirement = task
        await task.value
    }

    /// Stop and drop the shared Activity model: Window → Activity must
    /// not open a stopped model over a retired bridge and a closed
    /// archive. `install` starts a fresh one for the new content.
    func retireActivity() {
        activity?.stop()
        activity = nil
        activitySourceID = nil
    }

    func rebuildAfterRestore() async {
        await retireContent()
        do {
            let bridge = try await session.bridge()
            guard !backups.isStopping else { return }
            install(bridge)
        } catch {
            showArchiveStatus("Couldn't reopen the restored archive: \(error.localizedDescription)")
        }
    }

    func archiveClosed() async {
        await retireContent()
        showArchiveStatus("The archive is closed. Quit and reopen Sweetbriar to settle the restore.")
    }

    private func showArchiveStatus(_ message: String) {
        guard let window else { return }
        let frame = window.frame
        let content = NSView(frame: NSRect(origin: .zero, size: Self.defaultContentSize))
        window.contentViewController = nil
        statusLabel.removeFromSuperview()
        statusLabel.stringValue = message
        content.addSubview(statusLabel)
        NSLayoutConstraint.activate([
            statusLabel.centerXAnchor.constraint(equalTo: content.centerXAnchor),
            statusLabel.centerYAnchor.constraint(equalTo: content.centerYAnchor),
            statusLabel.widthAnchor.constraint(lessThanOrEqualTo: content.widthAnchor, constant: -40),
        ])
        window.contentView = content
        window.setFrame(frame, display: true)
    }

    /// Fence UI writers before the native restore is scheduled. Reader/list
    /// queries remain usable during preparation; fresh actions can resume after
    /// a refusal, but retired dialogs and queued writes cannot cross the swap.
    func retireWritersForRestore() {
        writes.retire()
        imports?.retire()
        imports = nil
        signIn?.retire()
        signIn = nil
        if let sheet = window?.attachedSheet {
            window?.endSheet(sheet)
            sheet.close()
        }
    }

    /// File → Add Proton Account…, or the sidebar's Sign In Again…
    /// for `account`, once the core is up (spec/08).
    func beginProtonSignIn(signingInAgain account: Account? = nil) {
        guard backups.canWrite else { return }
        configureWriters()
        signIn?.begin(signingInAgain: account)
    }

    /// The sidebar's health row asks for the Activity window (spec/08
    /// "Activity / account health").
    var onOpenActivity: (@MainActor () -> Void)?

    /// Tests hold the default Unified Inbox choice here, after the
    /// sidebar's first reload, to order it against an explicit choice.
    var beforeDefaultChoice: (@MainActor () async -> Void)?

    /// File → Import…, once the core is up.
    func beginImport() {
        guard backups.canWrite else { return }
        configureWriters()
        guard let imports else { return }
        writes.run(revision: writes.revision) { await imports.chooseAndImport() }
    }

    private func install(_ bridge: CoreBridge) {
        guard let window, let backupsModel = backups.model(source: bridge) else { return }
        if contentTasks.isRetired { contentTasks = ArchiveViewTasks() }
        retirement = nil
        contentSerial &+= 1
        let serial = contentSerial
        let activity = startActivity(bridge)
        let sidebarModel = SidebarModel(source: bridge)
        let appearanceModel = appearance.model(source: bridge)
        let sidebar = SidebarViewController(
            model: sidebarModel, appearance: appearanceModel, activity: activity)
        sidebar.onSignInAgain = { [weak self] account in self?.beginProtonSignIn(signingInAgain: account) }
        sidebar.onOpenActivity = { [weak self] in self?.onOpenActivity?() }
        let policy = backups
        sidebar.canWrite = { policy.canWrite }
        let messageList = MessageListViewController(bridge: bridge, sidebar: sidebarModel, appearance: appearanceModel,
                                                     writes: writes, canWrite: { policy.canWrite })
        let reader = ReaderViewController(
            model: ReaderModel(source: bridge, preferred: .sanitizedHtml),
            memory: readingViewMemory.model(source: bridge),
            appearance: appearanceModel
        )
        self.reader = reader
        let drafts = DraftsListViewController(
            model: DraftListModel(source: bridge, catalog: bridge), appearance: appearanceModel)
        let outBox = OutBoxViewController(
            model: OutBoxModel(source: bridge, catalog: bridge, canWrite: { policy.canWrite }),
            appearance: appearanceModel)
        let listPane = ListPaneController(messages: messageList, drafts: drafts, outBox: outBox)
        drafts.onOpen = { [weak self] id in
            guard let self, contentSerial == serial else { return }
            openDraft(id)
        }
        sidebar.onChoose = { [weak self, weak messageList, weak listPane] spec in
            guard let self, contentSerial == serial else { return }
            let wasNonMessage = listPane?.pane == .drafts || listPane?.pane == .outBox
            listPane?.show(.messages)
            messageList?.show(spec)
            if wasNonMessage, !closed { self.reader?.select(messageList?.list?.selectedIDs ?? []) }
        }
        sidebar.onChooseDrafts = { [weak self, weak drafts, weak listPane] in
            guard let self, contentSerial == serial else { return }
            listPane?.show(.drafts)
            self.reader?.select([])
            self.markRead?.show(nil)
            drafts?.reload()
        }
        sidebar.onChooseOutBox = { [weak self, weak outBox, weak listPane] in
            guard let self, contentSerial == serial else { return }
            listPane?.show(.outBox)
            self.reader?.select([])
            self.markRead?.show(nil)
            outBox?.reload()
        }
        messageList.onGeneration = { [weak self] generation in
            guard let self, contentSerial == serial else { return }
            listSaw(generation)
        }
        messageList.onSelectionChange = { [weak self] ids in
            guard let self, !closed, contentSerial == serial else { return }
            guard self.listPane?.pane == .messages else { return }
            self.reader?.select(ids)
            // One shown Proton row may be marked read; anything else,
            // or an archive that can't be written, ends a pending mark
            // (spec/08 "Reader").
            let targets = self.backups.canWrite ? self.flagTargets : nil
            self.markRead?.show(targets?.count == 1 ? targets?.first : nil)
        }
        messageList.onViewSaved = { [weak self] view in
            guard let self, contentSerial == serial else { return }
            self.selectSavedView(view)
        }

        let split = NSSplitViewController()
        let sidebarItem = NSSplitViewItem(sidebarWithViewController: sidebar)
        sidebarItem.minimumThickness = Self.sidebarMinimumWidth
        let listItem = NSSplitViewItem(contentListWithViewController: listPane)
        listItem.minimumThickness = Self.listMinimumWidth
        split.addSplitViewItem(sidebarItem)
        split.addSplitViewItem(listItem)
        split.addSplitViewItem(NSSplitViewItem(viewController: reader))
        let root = NSViewController()
        root.view = NSView()
        root.addChild(split)
        let banner = NSHostingView(rootView: IntegrityBanner(model: backupsModel, openBackups: openBackups))
        banner.sizingOptions = [.intrinsicContentSize]
        banner.setContentHuggingPriority(.required, for: .vertical)
        banner.translatesAutoresizingMaskIntoConstraints = false
        split.view.translatesAutoresizingMaskIntoConstraints = false
        root.view.addSubview(banner)
        root.view.addSubview(split.view)
        NSLayoutConstraint.activate([
            banner.topAnchor.constraint(equalTo: root.view.safeAreaLayoutGuide.topAnchor),
            banner.leadingAnchor.constraint(equalTo: root.view.leadingAnchor),
            banner.trailingAnchor.constraint(equalTo: root.view.trailingAnchor),
            split.view.topAnchor.constraint(equalTo: banner.bottomAnchor),
            split.view.leadingAnchor.constraint(equalTo: root.view.leadingAnchor),
            split.view.trailingAnchor.constraint(equalTo: root.view.trailingAnchor),
            split.view.bottomAnchor.constraint(equalTo: root.view.bottomAnchor),
        ])
        let filing = FilingController(
            source: bridge, window: window, list: messageList, sidebar: sidebarModel,
            appearance: appearanceModel, writes: writes, canWrite: { policy.canWrite },
            canFile: { [weak listPane] in policy.canWrite && listPane?.pane == .messages })
        self.filing?.retire()
        self.filing = filing
        sidebar.dropTarget = filing
        messageList.contextActions = { [weak self] ids in self?.contextActions(for: ids) ?? [] }
        messageList.filingUndoManager = filing.undoManager
        root.view.addSubview(filing.toastView)
        NSLayoutConstraint.activate([
            filing.toastView.centerXAnchor.constraint(equalTo: root.view.centerXAnchor),
            filing.toastView.bottomAnchor.constraint(
                equalTo: root.view.bottomAnchor, constant: -Self.toastMargin),
            filing.toastView.widthAnchor.constraint(
                lessThanOrEqualTo: root.view.widthAnchor, constant: -2 * Self.toastMargin),
        ])
        let frame = window.frame
        window.contentViewController = root
        window.setFrame(frame, display: true)

        self.bridge = bridge
        markRead?.cancel()
        markRead = MarkReadTimer(source: bridge)
        configureWriters()
        let imports = self.imports

        self.sidebarModel = sidebarModel
        self.sidebar = sidebar
        self.messageList = messageList
        self.drafts = drafts
        self.outBox = outBox
        self.listPane = listPane
        contentTasks.run { [weak self] in
            await sidebarModel.reload()
            guard let self, !Task.isCancelled, contentSerial == serial else { return }
            sidebar.reloadSections()
            await beforeDefaultChoice?()
            guard !Task.isCancelled, contentSerial == serial else { return }
            // A list chosen while the sidebar loaded stays chosen.
            if messageList.shownSpec == nil {
                sidebar.choose(entryID: SidebarModel.unifiedInboxID)
            }
            await imports?.reattach()
        }
    }

    /// The shared Activity model for this core session: started on the
    /// first install, restarted after a restore, and kept across
    /// rebuilds so an open Activity window keeps observing it.
    private func startActivity(_ bridge: CoreBridge) -> ActivityModel {
        let identity = ObjectIdentifier(bridge)
        if let activity, activitySourceID == identity {
            activity.start()
            return activity
        }
        activity?.stop()
        let model = ActivityModel(source: bridge)
        activitySourceID = identity
        activity = model
        model.start()
        return model
    }

    private func configureWriters() {
        guard let bridge, let window else { return }
        let serial = contentSerial
        if imports == nil {
            let owner = ImportCoordinator(bridge: bridge, window: window)
            owner.onArchiveChanged = { [weak self] in
                guard let self, contentSerial == serial else { return }
                reloadSidebar()
            }
            imports = owner
        }
        if signIn == nil {
            let policy = backups
            let owner = ProtonSignInSheet(window: window, source: bridge, writes: writes,
                                          canWrite: { policy.canWrite })
            owner.onAdded = { [weak self] in
                guard let self, contentSerial == serial else { return }
                reloadSidebar()
            }
            signIn = owner
        }
    }

    /// A compose window opened or closed: the drafts list, if shown,
    /// re-reads, and so does the Out Box, which a send changes (spec/08).
    func draftsChanged() {
        if listPane?.pane == .drafts { drafts?.reload() }
        if listPane?.pane == .outBox { outBox?.reload() }
    }

    /// Select a freshly saved view in the sidebar (spec/08 Search).
    private func selectSavedView(_ view: SavedView) {
        guard let sidebar else { return }
        let serial = contentSerial
        contentTasks.run { [weak self] in
            guard let self, contentSerial == serial else { return }
            sidebar.reloadSections()
            sidebar.choose(entryID: SidebarModel.savedViewEntryID(view.viewId))
        }
    }

    /// Re-read the sidebar: an import changed what it lists.
    private func reloadSidebar() {
        guard let sidebarModel, let sidebar else { return }
        let serial = contentSerial
        contentTasks.run { [weak self] in
            await sidebarModel.reload()
            guard let self, !Task.isCancelled, contentSerial == serial else { return }
            sidebar.reloadSections()
        }
    }

    /// A list saw `generation`: the sidebar re-reads if it shows an
    /// older one.
    private func listSaw(_ generation: UInt64) {
        guard let sidebarModel, let sidebar, generation > sidebarModel.generation else { return }
        let serial = contentSerial
        contentTasks.run { [weak self] in
            await sidebarModel.noteGeneration(generation)
            guard let self, !Task.isCancelled, contentSerial == serial else { return }
            sidebar.reloadSections()
        }
    }
}

// MARK: - Read and star (spec/08 "Message list" Keyboard, M3.2)

extension MainWindowController: MessageFlagCommands {
    /// The most rows one toggle acts on: `set_flags` takes at most
    /// 1,000 occurrence ids (spec/07 "Selections").
    private static let maxFlagSelection = 1_000

    /// The list's selected rows as the flag commands see them: nil
    /// unless the list shows messages and every selected row is loaded
    /// and belongs to a Proton account. The local archive's flags are
    /// later (spec/07 "Flags"), so a selection with one of its rows
    /// disables the commands.
    var flagTargets: [FlagTarget]? {
        messageList?.list.flatMap { flagTargets(for: $0.selectedIDs) }
    }

    /// The same for `ids`, the rows a context click targets.
    private func flagTargets(for ids: Set<Int64>) -> [FlagTarget]? {
        guard listPane?.pane == .messages, let model = messageList?.model, !ids.isEmpty,
            ids.count <= Self.maxFlagSelection
        else { return nil }
        var targets: [FlagTarget] = []
        for id in ids.sorted() {
            guard let index = model.index(of: id), let row = model.row(at: index),
                isProton(row.accountId)
            else { return nil }
            targets.append(FlagTarget(row))
        }
        return targets
    }

    private func isProton(_ accountId: String) -> Bool {
        sidebarModel?.sections.first { $0.id == accountId }?.account?.kind == .proton
    }

    /// Message → Mark as Read or Unread (⇧⌘U).
    @objc func toggleRead(_ sender: Any?) { toggle(.seen) }

    /// Message → Star or Unstar (⇧⌘L).
    @objc func toggleStar(_ sender: Any?) { toggle(.flagged) }

    private func toggle(_ flag: MessageFlag, ids: Set<Int64>? = nil) {
        guard backups.canWrite, let bridge, let targets = ids.map(flagTargets(for:)) ?? flagTargets,
            let toggle = FlagToggle.plan(flag, targets: targets)
        else {
            NSSound.beep()
            return
        }
        // Marking the shown message unread ends its pending mark, and
        // it stays unread until it is shown again.
        if flag == .seen { markRead?.cancel() }
        writes.run(revision: writes.revision) { [weak self] in
            let failures = await toggle.run(source: bridge)
            guard let self, let failure = failures.first, let window, !closed else { return }
            NSAlert(error: failure).beginSheetModal(for: window) { _ in }
        }
    }

    /// The commands' titles follow what they would do to the selection.
    func flagCommandState(
        _ action: Selector, ids: Set<Int64>? = nil
    ) -> (enabled: Bool, title: String)? {
        let targets = backups.canWrite ? (ids.map(flagTargets(for:)) ?? flagTargets) : nil
        switch action {
        case #selector(toggleRead(_:)):
            let read = targets.map { FlagToggle.plan(.seen, targets: $0)?.set ?? true } ?? true
            return (targets != nil, read ? "Mark as Read" : "Mark as Unread")
        case #selector(toggleStar(_:)):
            let star = targets.map { FlagToggle.plan(.flagged, targets: $0)?.set ?? true } ?? true
            return (targets != nil, star ? "Star" : "Unstar")
        default:
            return nil
        }
    }
}

// MARK: - The list's context menu (spec/08 "Selection")

extension MainWindowController {
    /// The flag and filing commands on the rows a context click
    /// targets, under the menu's identifiers.
    func contextActions(for ids: Set<Int64>) -> [MessageContextAction] {
        typealias Item = MainMenuBuilder.ItemID
        let read = flagCommandState(#selector(toggleRead(_:)), ids: ids)
        let star = flagCommandState(#selector(toggleStar(_:)), ids: ids)
        let files = filing?.canAct(on: ids) == true
        return [
            MessageContextAction(
                title: read?.title ?? "Mark as Read", identifier: Item.toggleRead,
                enabled: read?.enabled == true
            ) { [weak self] in self?.toggle(.seen, ids: ids) },
            MessageContextAction(
                title: star?.title ?? "Star", identifier: Item.toggleStar,
                enabled: star?.enabled == true
            ) { [weak self] in self?.toggle(.flagged, ids: ids) },
            MessageContextAction(title: "Archive", identifier: Item.archive, enabled: files) {
                [weak self] in self?.filing?.file(.archive, ids: ids)
            },
            MessageContextAction(title: "Move to Trash", identifier: Item.trash, enabled: files) {
                [weak self] in self?.filing?.file(.trash, ids: ids)
            },
            MessageContextAction(title: "Move to…", identifier: Item.move, enabled: files) {
                [weak self] in self?.filing?.choose(.move, ids: ids)
            },
            MessageContextAction(title: "Add Label…", identifier: Item.addLabel, enabled: files) {
                [weak self] in self?.filing?.choose(.addLabel, ids: ids)
            },
            MessageContextAction(title: "Remove Label…", identifier: Item.removeLabel, enabled: files) {
                [weak self] in self?.filing?.choose(.removeLabel, ids: ids)
            },
        ]
    }
}

// MARK: - Filing (spec/08 "Keyboard filing", M5.2)

extension MainWindowController: MessageFilingCommands {
    private static let filingActions: Set<Selector> = [
        #selector(archiveMessages(_:)), #selector(trashMessages(_:)), #selector(moveMessages(_:)),
        #selector(addLabel(_:)), #selector(removeLabel(_:)),
    ]

    @objc func archiveMessages(_ sender: Any?) { filing?.file(.archive) }

    @objc func trashMessages(_ sender: Any?) { filing?.file(.trash) }

    @objc func moveMessages(_ sender: Any?) { filing?.choose(.move) }

    @objc func addLabel(_ sender: Any?) { filing?.choose(.addLabel) }

    @objc func removeLabel(_ sender: Any?) { filing?.choose(.removeLabel) }

    /// Whether a filing command is live; nil for any other action.
    func filingCommandEnabled(_ action: Selector) -> Bool? {
        guard Self.filingActions.contains(action) else { return nil }
        return filing?.isEnabled == true
    }
}

extension MainWindowController: NSMenuItemValidation, NSToolbarItemValidation {
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if let action = menuItem.action, let enabled = filingCommandEnabled(action) { return enabled }
        guard let action = menuItem.action, let state = flagCommandState(action) else { return true }
        menuItem.title = state.title
        return state.enabled
    }

    func validateToolbarItem(_ item: NSToolbarItem) -> Bool {
        if let action = item.action, let enabled = filingCommandEnabled(action) { return enabled }
        guard let action = item.action, let state = flagCommandState(action) else { return true }
        item.label = state.title
        item.toolTip = state.title
        return state.enabled
    }
}

extension MainWindowController: NSToolbarDelegate {
    private static let readItem = NSToolbarItem.Identifier(MainMenuBuilder.ItemID.toggleRead.rawValue)
    private static let starItem = NSToolbarItem.Identifier(MainMenuBuilder.ItemID.toggleStar.rawValue)
    private static let archiveItem = NSToolbarItem.Identifier(MainMenuBuilder.ItemID.archive.rawValue)
    private static let trashItem = NSToolbarItem.Identifier(MainMenuBuilder.ItemID.trash.rawValue)
    private static let moveItem = NSToolbarItem.Identifier(MainMenuBuilder.ItemID.move.rawValue)
    private static let addLabelItem = NSToolbarItem.Identifier(MainMenuBuilder.ItemID.addLabel.rawValue)
    private static let removeLabelItem =
        NSToolbarItem.Identifier(MainMenuBuilder.ItemID.removeLabel.rawValue)
    private static var commandItems: [NSToolbarItem.Identifier] {
        [readItem, starItem, archiveItem, trashItem, moveItem, addLabelItem, removeLabelItem]
    }

    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.flexibleSpace] + Self.commandItems
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        [.flexibleSpace, .space] + Self.commandItems
    }

    func toolbar(
        _ toolbar: NSToolbar, itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        let spec: (title: String, symbol: String, action: Selector)
        switch itemIdentifier {
        case Self.readItem: spec = ("Mark as Read", "envelope.open", #selector(toggleRead(_:)))
        case Self.starItem: spec = ("Star", "star", #selector(toggleStar(_:)))
        case Self.archiveItem: spec = ("Archive", "archivebox", #selector(archiveMessages(_:)))
        case Self.trashItem: spec = ("Move to Trash", "trash", #selector(trashMessages(_:)))
        case Self.moveItem: spec = ("Move to…", "folder", #selector(moveMessages(_:)))
        case Self.addLabelItem: spec = ("Add Label…", "tag", #selector(addLabel(_:)))
        case Self.removeLabelItem: spec = ("Remove Label…", "tag.slash", #selector(removeLabel(_:)))
        default: return nil
        }
        let item = NSToolbarItem(itemIdentifier: itemIdentifier)
        item.label = spec.title
        item.paletteLabel = spec.title
        item.toolTip = spec.title
        item.image = NSImage(systemSymbolName: spec.symbol, accessibilityDescription: spec.title)
        item.action = spec.action
        item.target = nil
        item.autovalidates = true
        return item
    }
}
