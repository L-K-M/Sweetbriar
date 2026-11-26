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
    /// Room for the sidebar, a list and a narrow reader.
    static let minimumContentSize = NSSize(width: 800, height: 420)
    private static let frameAutosaveName = "MainWindow"
    private static let splitAutosaveName = "MainWindowSplit"
    /// The window's title, also how UI tests find it.
    static let title = "Sweetbriar"
    private static let sidebarMinimumWidth: CGFloat = 180
    private static let listMinimumWidth: CGFloat = 320
    private static let readerMinimumWidth: CGFloat = 280
    private static let toastMargin: CGFloat = 20

    private let session: CoreSession
    private let appearance: SharedAppearance
    /// The compose cue mode, which also tints message windows (spec/08
    /// Message window "Tint").
    private let cues: SharedComposeCue?
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

    /// What Reply, Reply All and Forward answer: the key message
    /// window's message, else the list's single selection (spec/08
    /// "Message window").
    var replyOccurrenceId: Int64? {
        messageWindows.occurrence(of: NSApp.keyWindow) ?? selectedOccurrenceId
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
    /// The flags last read for a whole-window selection, for the flag
    /// commands' titles.
    private var windowFlags: (selection: MessageSelection, flags: SelectionFlags)?
    /// Messages opened in their own windows (spec/08 "Message window").
    private let messageWindows = MessageWindows()
    private(set) var filing: FilingController?
    private var closed = false
    /// The split of the installed content, for View → Show Sidebar.
    private var split: NSSplitViewController?
    /// The integrity banner, under the toolbar across the window.
    private var banner: IntegrityBannerAccessory?
    /// The toolbar's search item; its field is the installed list's.
    private weak var searchItem: NSSearchToolbarItem?
    /// The installed list's row count, for the subtitle.
    private var shownCount: Int?
    /// Targets New Message, Reply, Reply All and Forward in the toolbar:
    /// the menu builder, so both take the same admission.
    private weak var composeCommands: MainMenuBuilder?


    init(session: CoreSession, appearance: SharedAppearance, cues: SharedComposeCue? = nil,
         backups: SharedBackups, readingViewMemory: SharedReadingViewMemory,
         openBackups: @escaping @MainActor () -> Void = {},
         openDraft: @escaping @MainActor (String) -> Void = { _ in },
         composeCommands: MainMenuBuilder? = nil) {
        self.session = session
        self.composeCommands = composeCommands
        self.appearance = appearance
        self.cues = cues
        self.backups = backups
        self.readingViewMemory = readingViewMemory
        self.openBackups = openBackups
        self.openDraft = openDraft
        // Full-size content: the sidebar runs the window's height under
        // the toolbar, as a Liquid Glass sidebar; the list and reader
        // panes keep their content below it through the safe area.
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.defaultContentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
            backing: .buffered,
            defer: false
        )
        window.title = Self.title
        // UI tests and accessibility find the window by this identifier
        // while its title follows the shown mailbox.
        window.setAccessibilityIdentifier(Self.title)
        window.toolbarStyle = .unified
        window.contentMinSize = Self.minimumContentSize
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
        toolbar.allowsUserCustomization = true
        toolbar.autosavesConfiguration = true
        window.toolbar = toolbar
        window.center()
        // Restores the last frame, if one was saved.
        window.setFrameAutosaveName(Self.frameAutosaveName)
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
        windowFlags = nil
        pendingAccountReveal = nil
        messageWindows.closeAll()
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
        retireChrome()
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
    var onComposeFromMessage: (@MainActor (DraftSource) -> Void)?

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
            self.updateTitle()
        }
        sidebar.onChooseDrafts = { [weak self, weak drafts, weak listPane] in
            guard let self, contentSerial == serial else { return }
            listPane?.show(.drafts)
            self.reader?.select([])
            self.markRead?.show(nil)
            drafts?.reload()
            self.updateTitle()
        }
        sidebar.onChooseOutBox = { [weak self, weak outBox, weak listPane] in
            guard let self, contentSerial == serial else { return }
            listPane?.show(.outBox)
            self.reader?.select([])
            self.markRead?.show(nil)
            outBox?.reload()
            self.updateTitle()
        }
        messageList.onCountChange = { [weak self] count in
            guard let self, contentSerial == serial else { return }
            shownCount = count
            updateTitle()
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
            self.refreshWindowFlags()
        }
        messageList.onOpenMessage = { [weak self] id in
            guard let self, !closed, contentSerial == serial else { return }
            openMessageWindow(id)
        }
        messageList.onPageReader = { [weak self] page in
            guard let self, !closed, contentSerial == serial else { return }
            self.reader?.page(page)
        }
        messageList.onViewSaved = { [weak self] view in
            guard let self, contentSerial == serial else { return }
            self.selectSavedView(view)
        }

        let split = NSSplitViewController()
        let sidebarItem = NSSplitViewItem(sidebarWithViewController: sidebar)
        sidebarItem.minimumThickness = Self.sidebarMinimumWidth
        sidebarItem.canCollapse = true
        let listItem = NSSplitViewItem(contentListWithViewController: listPane)
        listItem.minimumThickness = Self.listMinimumWidth
        let readerItem = NSSplitViewItem(viewController: reader)
        readerItem.minimumThickness = Self.readerMinimumWidth
        split.addSplitViewItem(sidebarItem)
        split.addSplitViewItem(listItem)
        split.addSplitViewItem(readerItem)
        // Pane widths persist with the window's frame.
        split.splitView.autosaveName = Self.splitAutosaveName
        let root = NSViewController()
        root.view = NSView()
        root.addChild(split)
        split.view.translatesAutoresizingMaskIntoConstraints = false
        root.view.addSubview(split.view)
        // The split fills the window, under the toolbar: the sidebar's
        // glass runs its full height, and each pane's safe area keeps its
        // content below the toolbar.
        NSLayoutConstraint.activate([
            split.view.topAnchor.constraint(equalTo: root.view.topAnchor),
            split.view.leadingAnchor.constraint(equalTo: root.view.leadingAnchor),
            split.view.trailingAnchor.constraint(equalTo: root.view.trailingAnchor),
            split.view.bottomAnchor.constraint(equalTo: root.view.bottomAnchor),
        ])
        self.split = split
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
        // The integrity banner spans the window under the toolbar and
        // collapses while there is nothing to say (spec/08 "Backups").
        // Added once the content is in, so its first measure can't
        // resize the window around the old status view.
        let banner = IntegrityBannerAccessory(model: backupsModel, openBackups: openBackups)
        self.banner?.removeFromParent()
        window.addTitlebarAccessoryViewController(banner)
        self.banner = banner
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
        // The toolbar's search item edits this list's search.
        searchItem?.searchField = messageList.searchField
        messageList.searchField.isEnabled = true
        messageList.onSearchCancel = { [weak self] in self?.searchItem?.endSearchInteraction() }
        contentTasks.run { [weak self] in
            await sidebarModel.reload()
            guard let self, !Task.isCancelled, contentSerial == serial else { return }
            sidebar.reloadSections()
            updateTitle()
            await beforeDefaultChoice?()
            guard !Task.isCancelled, contentSerial == serial else { return }
            // A list chosen while the sidebar loaded stays chosen.
            if messageList.shownSpec == nil {
                sidebar.choose(entryID: SidebarModel.unifiedInboxID, revealSection: false)
            }
            // A notification tapped during the open lands after the
            // sidebar exists.
            revealPendingAccountInbox()
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

    /// Open `accountId`'s inbox, used when a new-mail notification is
    /// tapped. The account's Inbox role wins; otherwise its first
    /// selectable mailbox. A tap that arrives while the archive is still
    /// opening is remembered and consumed once the sidebar exists; one
    /// that names no listed destination after one reload is dropped.
    func revealAccountInbox(accountId: String) {
        pendingAccountReveal = accountId
        revealPendingAccountInbox()
    }

    private func revealPendingAccountInbox() {
        guard !closed, let accountId = pendingAccountReveal,
            let sidebarModel, let sidebar
        else { return }
        let serial = contentSerial
        contentTasks.run { [weak self] in
            guard let self, contentSerial == serial else { return }
            if self.sidebarModel?.inboxEntryID(accountId: accountId) == nil {
                await sidebarModel.reload()
                guard !Task.isCancelled, contentSerial == serial else { return }
                sidebar.reloadSections()
                updateTitle()
            }
            guard let entryID = sidebarModel.inboxEntryID(accountId: accountId) else {
                self.pendingAccountReveal = nil
                return
            }
            self.pendingAccountReveal = nil
            sidebar.choose(entryID: entryID)
        }
    }

    /// A notification's account whose inbox is not open yet; a tap during
    /// a cold launch is served once `install` has built the sidebar.
    private var pendingAccountReveal: String?

    /// Re-read the sidebar: an import changed what it lists.
    private func reloadSidebar() {
        guard let sidebarModel, let sidebar else { return }
        let serial = contentSerial
        contentTasks.run { [weak self] in
            await sidebarModel.reload()
            guard let self, !Task.isCancelled, contentSerial == serial else { return }
            sidebar.reloadSections()
            updateTitle()
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
            updateTitle()
        }
    }
}

// MARK: - Title, banner and search field (spec/08 "Windows")

extension MainWindowController {
    /// The window's title names what the list pane shows, as Mail's
    /// does, and its subtitle the shown list's size: the account and
    /// the message and unread counts. The Window menu, Mission Control
    /// and Stage Manager show them too.
    func updateTitle() {
        guard let window else { return }
        let presentation = Self.titlePresentation(
            pane: listPane?.pane, searching: messageList?.isShowingSearch == true,
            entry: sidebar?.chosenEntry, account: sidebar?.chosenAccountTitle, count: shownCount)
        if window.title != presentation.title { window.title = presentation.title }
        if window.subtitle != presentation.subtitle { window.subtitle = presentation.subtitle }
    }

    /// The title and subtitle for what the list pane shows; free of the
    /// window's state for tests.
    static func titlePresentation(
        pane: ListPaneController.Pane?, searching: Bool, entry: SidebarModel.Entry?,
        account: String?, count: Int?
    ) -> (title: String, subtitle: String) {
        switch pane {
        case .drafts: return ("Drafts (local)", "")
        case .outBox: return ("Out Box", "")
        case .messages, nil: break
        }
        if searching {
            return ("Search", count.map { $0 == 1 ? "1 result" : "\($0.formatted()) results" } ?? "")
        }
        guard let entry else { return (Self.title, "") }
        var parts: [String] = []
        if let account { parts.append(account) }
        if let count {
            var counted = count == 1 ? "1 message" : "\(count.formatted()) messages"
            if let unread = entry.unread, unread > 0 { counted += ", \(unread.formatted()) unread" }
            parts.append(counted)
        }
        return (entry.title, parts.joined(separator: " · "))
    }

    /// Edit → Find → Search Mail (⌥⌘F): into the toolbar's search
    /// field, expanding it if the toolbar collapsed it.
    @objc func focusSearch(_ sender: Any?) {
        guard let searchItem, messageList != nil, window?.toolbar?.isVisible == true else {
            return NSSound.beep()
        }
        searchItem.beginSearchInteraction()
    }

    /// View → Show or Hide Sidebar, and the toolbar's sidebar button,
    /// also while nothing in the window has the keyboard.
    @objc func toggleSidebar(_ sender: Any?) {
        guard let split else { return NSSound.beep() }
        split.toggleSidebar(sender)
    }

    /// Drop what the retired content showed: its banner, its search
    /// field and the title.
    func retireChrome() {
        banner?.removeFromParent()
        banner = nil
        split = nil
        shownCount = nil
        searchItem?.searchField = Self.placeholderSearchField()
        window?.title = Self.title
        window?.subtitle = ""
    }

    /// The toolbar's field before a list is installed: present, so the
    /// toolbar keeps its shape, but not editable.
    static func placeholderSearchField() -> NSSearchField {
        let field = NSSearchField()
        field.placeholderString = "Search"
        field.isEnabled = false
        return field
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

    /// A mailbox window's whole selection past the explicit limit, as
    /// its `SelectionToken`, when that mailbox is a Proton account's
    /// (spec/08 "Message list" Keyboard). Nil for anything else.
    var flagWindowSelection: MessageSelection? {
        messageList?.list.flatMap { flagWindowSelection(for: $0.selectedIDs) }
    }

    /// The same for `ids`, the rows a context click targets: a context
    /// click on a selected row acts on the selection (spec/08
    /// "Selection").
    private func flagWindowSelection(for ids: Set<Int64>) -> MessageSelection? {
        guard listPane?.pane == .messages, let model = messageList?.model,
            ids.count > Self.maxFlagSelection,
            let selection = model.wholeWindowSelection(of: ids),
            let row = model.row(at: 0), isProton(row.accountId)
        else { return nil }
        return selection
    }

    /// Read the whole-window selection's flags for the commands'
    /// titles; the command itself reads them again when it runs.
    private func refreshWindowFlags() {
        guard let selection = flagWindowSelection, let bridge else {
            windowFlags = nil
            return
        }
        if windowFlags?.selection == selection { return }
        windowFlags = nil
        contentTasks.run { [weak self] in
            guard let flags = try? await bridge.selectionFlags(selection: selection) else { return }
            guard let self, !closed, flagWindowSelection == selection else { return }
            windowFlags = (selection, flags)
        }
    }

    private func isProton(_ accountId: String) -> Bool {
        sidebarModel?.sections.first { $0.id == accountId }?.account?.kind == .proton
    }

    /// Message → Mark as Read or Unread (⇧⌘U).
    @objc func toggleRead(_ sender: Any?) { toggle(.seen) }

    /// Message → Star or Unstar (⇧⌘L).
    @objc func toggleStar(_ sender: Any?) { toggle(.flagged) }

    private func toggle(_ flag: MessageFlag, ids: Set<Int64>? = nil) {
        let targets = ids.map(flagTargets(for:)) ?? flagTargets
        let whole = ids.map(flagWindowSelection(for:)) ?? flagWindowSelection
        if backups.canWrite, targets == nil, let bridge, let selection = whole {
            toggleWindow(flag, selection: selection, bridge: bridge)
            return
        }
        guard backups.canWrite, let bridge, let targets,
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

    /// One call with the window's token; its direction comes from the
    /// core when it runs, so rows the list never loaded count too.
    private func toggleWindow(_ flag: MessageFlag, selection: MessageSelection, bridge: CoreBridge) {
        if flag == .seen { markRead?.cancel() }
        windowFlags = nil
        let toggle = WindowFlagToggle(flag: flag, selection: selection)
        writes.run(revision: writes.revision) { [weak self] in
            let failure = await toggle.run(source: bridge)
            guard let self, !closed else { return }
            refreshWindowFlags()
            guard let failure, let window else { return }
            NSAlert(error: failure).beginSheetModal(for: window) { _ in }
        }
    }

    /// The commands' titles follow what they would do to the selection.
    /// A whole-window selection takes its title from the flags last
    /// read for it, and reads as setting until they arrive.
    func flagCommandState(
        _ action: Selector, ids: Set<Int64>? = nil
    ) -> (enabled: Bool, title: String)? {
        let targets = backups.canWrite ? (ids.map(flagTargets(for:)) ?? flagTargets) : nil
        let window =
            backups.canWrite && targets == nil
            ? (ids.map(flagWindowSelection(for:)) ?? flagWindowSelection) : nil
        let flags = window.flatMap { selection in
            windowFlags?.selection == selection ? windowFlags?.flags : nil
        }
        let enabled = targets != nil || window != nil
        switch action {
        case #selector(toggleRead(_:)):
            let read =
                targets.map { FlagToggle.plan(.seen, targets: $0)?.set ?? true }
                ?? flags.map { WindowFlagToggle.sets(.seen, $0) } ?? true
            return (enabled, read ? "Mark as Read" : "Mark as Unread")
        case #selector(toggleStar(_:)):
            let star =
                targets.map { FlagToggle.plan(.flagged, targets: $0)?.set ?? true }
                ?? flags.map { WindowFlagToggle.sets(.flagged, $0) } ?? true
            return (enabled, star ? "Star" : "Unstar")
        default:
            return nil
        }
    }
}

// MARK: - The list's context menu (spec/08 "Selection")

extension MainWindowController {
    /// The commands on the rows a context click targets, under the
    /// menu's identifiers. Single-message commands use the click target.
    func contextActions(for ids: Set<Int64>) -> [MessageContextAction] {
        typealias Item = MainMenuBuilder.ItemID
        let read = flagCommandState(#selector(toggleRead(_:)), ids: ids)
        let star = flagCommandState(#selector(toggleStar(_:)), ids: ids)
        let files = filing?.canAct(on: ids) == true
        let messageActions: [MessageContextAction] =
            if ids.count == 1, let id = ids.first { singleMessageContextActions(for: id) }
            else { [] }
        return messageActions + [
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

    private func singleMessageContextActions(for id: Int64) -> [MessageContextAction] {
        typealias Item = MainMenuBuilder.ItemID
        let open = MessageContextAction(title: "Open in New Window", identifier: Item.openMessage, enabled: bridge != nil) {
            [weak self] in self?.openMessageWindow(id)
        }
        let canCompose = backups.canCompose && onComposeFromMessage != nil
        let draftCommands: [(String, NSUserInterfaceItemIdentifier, DraftSource)] = [
            ("Reply", Item.reply, .reply(occurrenceId: id)),
            ("Reply All", Item.replyAll, .replyAll(occurrenceId: id)),
            ("Forward", Item.forward, .forward(occurrenceId: id)),
        ]
        return [open] + draftCommands.map { title, identifier, source in
            MessageContextAction(title: title, identifier: identifier, enabled: canCompose) {
                [weak self] in self?.onComposeFromMessage?(source)
            }
        }
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

// MARK: - Message windows (spec/08 "Message window")

extension MainWindowController: MessageWindowCommands {
    /// Message → Open in New Window (⌘O): the list's single selection.
    @objc func openMessage(_ sender: Any?) {
        guard let id = selectedOccurrenceId else {
            NSSound.beep()
            return
        }
        openMessageWindow(id)
    }

    /// Open `id` in its own window, or bring its window to the front.
    /// The row the list showed decides whether it is marked read.
    func openMessageWindow(_ id: Int64) {
        guard let bridge else { return }
        // Built only for a new window; an open one just comes forward.
        messageWindows.open(id) {
            let memory = readingViewMemory.model(source: bridge)
            let appearanceModel = appearance.model(source: bridge)
            return MessageWindowController(
                occurrenceId: id,
                reader: ReaderViewController(
                    model: ReaderModel(source: bridge, preferred: .sanitizedHtml), memory: memory,
                    appearance: appearanceModel),
                markRead: backups.canWrite ? MarkReadTimer(source: bridge) : nil,
                target: flagTargets(for: [id])?.first,
                appearance: appearanceModel, cue: cues?.model(source: bridge))
        }
    }
}

// MARK: - View and Mailbox commands

extension MainWindowController: ReaderCommands, MailboxCommands {
    /// The reader, while the list pane shows messages: Drafts (local)
    /// and the Out Box leave it empty (spec/08).
    private var messageReader: ReaderViewController? {
        listPane?.pane == .messages ? reader : nil
    }

    /// View → Message Source (⌥⌘U).
    @objc func toggleMessageSource(_ sender: Any?) {
        guard let reader = messageReader, reader.canToggleSource else { return NSSound.beep() }
        reader.toggleSourceFromMenu()
    }

    /// View → Reading View (⌥⌘R).
    @objc func toggleReadingView(_ sender: Any?) {
        guard let reader = messageReader, reader.canToggleReadingView else { return NSSound.beep() }
        reader.toggleReadingViewFromMenu()
    }

    /// Mailbox → Inbox, Flagged, Drafts (local) or Out Box (⌘1–⌘4):
    /// chosen as a click in the sidebar would.
    @objc func goToMailbox(_ sender: Any?) {
        guard let id = (sender as? NSMenuItem)?.representedObject as? String, let sidebar,
            sidebarModel?.entry(id: id) != nil
        else { return NSSound.beep() }
        sidebar.choose(entryID: id)
    }

    /// Whether a View or Mailbox command is live, checking the reader's
    /// two renditions; nil for any other action.
    private func viewCommandEnabled(_ menuItem: NSMenuItem) -> Bool? {
        switch menuItem.action {
        case #selector(toggleMessageSource(_:)):
            menuItem.state = messageReader?.isShowingSource == true ? .on : .off
            return messageReader?.canToggleSource == true
        case #selector(toggleReadingView(_:)):
            menuItem.state = messageReader?.isShowingReadingView == true ? .on : .off
            return messageReader?.canToggleReadingView == true
        case #selector(goToMailbox(_:)):
            guard let id = menuItem.representedObject as? String else { return false }
            return sidebar != nil && sidebarModel?.entry(id: id) != nil
        default:
            return nil
        }
    }
}

extension MainWindowController: NSMenuItemValidation, NSToolbarItemValidation {
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(MessageWindowCommands.openMessage(_:)) {
            return selectedOccurrenceId != nil
        }
        if let enabled = viewCommandEnabled(menuItem) { return enabled }
        if let action = menuItem.action, let enabled = filingCommandEnabled(action) { return enabled }
        guard let action = menuItem.action, let state = flagCommandState(action) else { return true }
        menuItem.title = state.title
        return state.enabled
    }

    func validateToolbarItem(_ item: NSToolbarItem) -> Bool {
        if let action = item.action, let enabled = filingCommandEnabled(action) { return enabled }
        if item.action == #selector(toggleSidebar(_:)) { return split != nil }
        guard let action = item.action, let state = flagCommandState(action) else { return true }
        item.label = state.title
        item.toolTip = state.title
        // The symbol says what the click does, as the label does.
        let symbol = MainToolbar.flagSymbol(for: state.title)
        if let symbol, item.image?.accessibilityDescription != state.title {
            item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: state.title)
        }
        return state.enabled
    }
}

extension MainWindowController: NSToolbarDelegate {
    func toolbarDefaultItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        MainToolbar.defaultItems
    }

    func toolbarAllowedItemIdentifiers(_ toolbar: NSToolbar) -> [NSToolbarItem.Identifier] {
        MainToolbar.allowedItems
    }

    func toolbar(
        _ toolbar: NSToolbar, itemForItemIdentifier itemIdentifier: NSToolbarItem.Identifier,
        willBeInsertedIntoToolbar flag: Bool
    ) -> NSToolbarItem? {
        if itemIdentifier == MainToolbar.search {
            let item = NSSearchToolbarItem(itemIdentifier: itemIdentifier)
            item.label = "Search"
            item.paletteLabel = "Search"
            item.toolTip = "Search mail (⌥⌘F)"
            item.preferredWidthForSearchField = MainToolbar.searchFieldWidth
            item.searchField = messageList?.searchField ?? Self.placeholderSearchField()
            if flag { searchItem = item }
            return item
        }
        return MainToolbar.item(itemIdentifier, composeTarget: composeCommands)
    }
}
