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
import SweetbriarBridge

@MainActor
final class MainWindowController: NSWindowController, NSWindowDelegate {
    static let defaultContentSize = NSSize(width: 1100, height: 700)
    /// The window's title, also how UI tests find it.
    static let title = "Sweetbriar"
    private static let sidebarMinimumWidth: CGFloat = 180
    private static let listMinimumWidth: CGFloat = 320

    private let session: CoreSession
    private let statusLabel = NSTextField(labelWithString: "Opening the archive…")
    private(set) var sidebarModel: SidebarModel?
    private(set) var sidebar: SidebarViewController?
    private(set) var messageList: MessageListViewController?
    private var imports: ImportCoordinator?
    private var reader: ReaderViewController?
    private var closed = false


    init(session: CoreSession) {
        self.session = session
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
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                let bridge = try await session.bridge()
                install(bridge)
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

    /// File → Import…, once the core is up.
    func beginImport() {
        guard let imports else { return }
        Task { @MainActor in
            await imports.chooseAndImport()
        }
    }

    private func install(_ bridge: CoreBridge) {
        guard let window else { return }
        let sidebarModel = SidebarModel(source: bridge)
        let sidebar = SidebarViewController(model: sidebarModel)
        let messageList = MessageListViewController(bridge: bridge)
        let reader = ReaderViewController(model: ReaderModel(source: bridge, preferred: .sanitizedHtml))
        self.reader = reader
        sidebar.onChoose = { [weak messageList] spec in
            guard let messageList else { return }
            Task { @MainActor in
                await messageList.show(spec)
            }
        }
        messageList.onGeneration = { [weak self] generation in
            self?.listSaw(generation)
        }
        messageList.onSelectionChange = { [weak self] ids in
            guard let self, !self.closed else { return }
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
        let frame = window.frame
        window.contentViewController = split
        window.setFrame(frame, display: true)

        let imports = ImportCoordinator(bridge: bridge, window: window)
        imports.onArchiveChanged = { [weak self] in
            self?.reloadSidebar()
        }

        self.sidebarModel = sidebarModel
        self.sidebar = sidebar
        self.messageList = messageList
        self.imports = imports
        Task { @MainActor in
            await sidebarModel.reload()
            sidebar.reloadSections()
            sidebar.choose(entryID: SidebarModel.unifiedInboxID)
            await imports.reattach()
        }
    }

    /// Re-read the sidebar: an import changed what it lists.
    private func reloadSidebar() {
        guard let sidebarModel, let sidebar else { return }
        Task { @MainActor in
            await sidebarModel.reload()
            sidebar.reloadSections()
        }
    }

    /// A list saw `generation`: the sidebar re-reads if it shows an
    /// older one.
    private func listSaw(_ generation: UInt64) {
        guard let sidebarModel, let sidebar, generation > sidebarModel.generation else { return }
        Task { @MainActor in
            await sidebarModel.noteGeneration(generation)
            sidebar.reloadSections()
        }
    }
}
