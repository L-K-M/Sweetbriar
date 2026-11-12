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

    private let session: CoreSession
    private let appearance: SharedAppearance
    private let backups: SharedBackups
    private let openBackups: @MainActor () -> Void
    private let writes = ArchiveWriteTasks()
    private var bridge: CoreBridge?
    private var contentTasks = ArchiveViewTasks()
    private var contentSerial: UInt64 = 0
    private var retirement: Task<Void, Never>?
    private let statusLabel = NSTextField(labelWithString: "Opening the archive…")
    private(set) var sidebarModel: SidebarModel?
    private(set) var sidebar: SidebarViewController?
    private(set) var messageList: MessageListViewController?
    private var imports: ImportCoordinator?
    private var accountSetup: BridgeAccountSheet?
    private var reader: ReaderViewController?
    private var closed = false


    init(session: CoreSession, appearance: SharedAppearance, backups: SharedBackups,
         openBackups: @escaping @MainActor () -> Void = {}) {
        self.session = session
        self.appearance = appearance
        self.backups = backups
        self.openBackups = openBackups
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.defaultContentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable, .fullSizeContentView],
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
        reader?.select(messageList?.list?.selectedIDs ?? [])
    }

    func windowWillClose(_ notification: Notification) {
        closed = true
        // Dock reopen reuses this controller, so release without retiring its model.
        reader?.select([])
    }

    /// Retire all old-context work before the shell opens any new handles.
    func retireContent() async {
        if let retirement { await retirement.value; return }
        contentSerial &+= 1
        contentTasks.retire()
        sidebar?.onChoose = nil
        messageList?.onGeneration = nil
        messageList?.onSelectionChange = nil
        retireWritersForRestore()
        let list = messageList
        let reader = self.reader
        reader?.close()
        sidebarModel = nil
        sidebar = nil
        messageList = nil
        self.reader = nil
        imports = nil
        accountSetup = nil
        showArchiveStatus("Updating the archive…")
        let tasks = contentTasks
        let writes = self.writes
        let task = Task {
            await writes.waitForRetirement()
            await list?.close()
            await reader?.closeAndWait()
            await tasks.waitForRetirement()
        }
        retirement = task
        await task.value
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
        accountSetup?.retire()
        accountSetup = nil
        if let sheet = window?.attachedSheet {
            window?.endSheet(sheet)
            sheet.close()
        }
    }

    /// File → Add Proton Bridge Account…, once the core is up.
    func beginBridgeAccountSetup() {
        guard backups.canWrite else { return }
        configureWriters()
        accountSetup?.begin()
    }

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
        let sidebarModel = SidebarModel(source: bridge)
        let appearanceModel = appearance.model(source: bridge)
        let sidebar = SidebarViewController(model: sidebarModel, appearance: appearanceModel)
        let policy = backups
        let messageList = MessageListViewController(bridge: bridge, sidebar: sidebarModel, appearance: appearanceModel,
                                                     writes: writes, canWrite: { policy.canWrite })
        let reader = ReaderViewController(model: ReaderModel(source: bridge, preferred: .sanitizedHtml))
        self.reader = reader
        sidebar.onChoose = { [weak messageList] spec in
            messageList?.show(spec)
        }
        messageList.onGeneration = { [weak self] generation in
            guard let self, contentSerial == serial else { return }
            listSaw(generation)
        }
        messageList.onSelectionChange = { [weak self] ids in
            guard let self, !closed, contentSerial == serial else { return }
            self.reader?.select(ids)
        }

        let split = NSSplitViewController()
        let sidebarItem = NSSplitViewItem(sidebarWithViewController: sidebar)
        sidebarItem.minimumThickness = Self.sidebarMinimumWidth
        let listItem = NSSplitViewItem(contentListWithViewController: messageList)
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
            banner.topAnchor.constraint(equalTo: root.view.topAnchor),
            banner.leadingAnchor.constraint(equalTo: root.view.leadingAnchor),
            banner.trailingAnchor.constraint(equalTo: root.view.trailingAnchor),
            split.view.topAnchor.constraint(equalTo: banner.bottomAnchor),
            split.view.leadingAnchor.constraint(equalTo: root.view.leadingAnchor),
            split.view.trailingAnchor.constraint(equalTo: root.view.trailingAnchor),
            split.view.bottomAnchor.constraint(equalTo: root.view.bottomAnchor),
        ])
        let frame = window.frame
        window.contentViewController = root
        window.setFrame(frame, display: true)

        self.bridge = bridge
        configureWriters()
        let imports = self.imports

        self.sidebarModel = sidebarModel
        self.sidebar = sidebar
        self.messageList = messageList
        contentTasks.run { [weak self] in
            await sidebarModel.reload()
            guard let self, !Task.isCancelled, contentSerial == serial else { return }
            sidebar.reloadSections()
            sidebar.choose(entryID: SidebarModel.unifiedInboxID)
            await imports?.reattach()
        }
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
        if accountSetup == nil {
            let policy = backups
            let owner = BridgeAccountSheet(window: window, source: bridge, writes: writes,
                                           canWrite: { policy.canWrite })
            owner.onAdded = { [weak self] in
                guard let self, contentSerial == serial else { return }
                reloadSidebar()
            }
            accountSetup = owner
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
