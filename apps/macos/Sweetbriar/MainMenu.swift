// The main menu, built in code — the app has no nib (spec/08 "Debug
// menu (pilot)"). The Debug menu ships in every build so the owner's
// checklist runs on the CI artifact: the synthetic fill, the list
// prototype, the implementation toggle and the benchmark stub part
// B2b wires.

import AppKit
import SweetbriarBridge

/// Which framework the prototype window lists with (spec/08 "Debug
/// menu"), persisted in user defaults under one key; AppKit is the
/// default.
enum PrototypeImplementation: String, CaseIterable {
    /// The user-defaults key "List Implementation" persists under
    /// (spec/08 "Debug menu").
    private static let defaultsKey = "debug.listPrototype.implementation"

    case appkit
    case swiftui

    /// The menu item's and the window title's name.
    var displayName: String {
        switch self {
        case .appkit:
            return "AppKit"
        case .swiftui:
            return "SwiftUI"
        }
    }

    /// The stored choice, or `.appkit` when unset or unknown.
    static var stored: PrototypeImplementation {
        get {
            guard let raw = UserDefaults.standard.string(
                forKey: Self.defaultsKey
            ) else {
                return .appkit
            }
            return PrototypeImplementation(rawValue: raw) ?? .appkit
        }
        set {
            UserDefaults.standard.set(
                newValue.rawValue,
                forKey: Self.defaultsKey
            )
        }
    }
}

/// Builds the whole main menu and owns the Debug items' actions.
///
/// AppDelegate injects what each command does and retains the builder
/// for the app's life — `NSMenuItem.target` is weak, so a menu without
/// a live builder goes dead.
@MainActor
final class MainMenuBuilder: NSObject {

    /// The fill count "Fill 500,000 Synthetic Rows" requests (plan
    /// M0.10, spec/07 "Debug source").
    static let syntheticFillCount: UInt32 = 500_000

    private let onFillSynthetic: @MainActor () -> Void
    private let onShowPrototype: @MainActor () -> Void
    private let onImplementationChanged: @MainActor (PrototypeImplementation) -> Void
    private let onRunBenchmark: @MainActor () -> Void
    private let onRunStoreBenchmark: @MainActor () -> Void
    private let onRunSearchBenchmark: @MainActor () -> Void
    private let isBenchmarkEnabled: @MainActor () -> Bool
    private let onShowComposerProbe: @MainActor () -> Void
    private let onImport: @MainActor () -> Void
    private let onNewMessage: @MainActor () -> Void
    private let isComposeEnabled: @MainActor () -> Bool
    private let onReply: @MainActor () -> Void
    private let onReplyAll: @MainActor () -> Void
    private let onForward: @MainActor () -> Void
    private let isReplyEnabled: @MainActor () -> Bool
    private let onAddProtonAccount: @MainActor () -> Void
    private let onSettings: @MainActor () -> Void
    private let onShowActivity: @MainActor () -> Void
    private let isWriteEnabled: @MainActor () -> Bool
    /// The opt-in latency log the Debug items control (spec/08).
    private let latency: LatencyRecorder
    private var latencyReport: BenchmarkReportWindowController?

    /// Every command's stable identifier, in the `ch.lkmc.Sweetbriar.cmd.*`
    /// namespace (spec/08 "Menus and commands"). AppKit's own items
    /// (About, Hide, Hide Others, Show All, Quit, Undo, Redo, Cut, Copy,
    /// Paste, Select All, Minimize, Close) carry none: they are AppKit's
    /// commands, not Sweetbriar's.
    enum ItemID {
        static func command(_ name: String) -> NSUserInterfaceItemIdentifier {
            NSUserInterfaceItemIdentifier("ch.lkmc.Sweetbriar.cmd.\(name)")
        }

        /// Sweetbriar → Settings….
        static let settings = command("settings")
        /// File → New Message.
        static let newMessage = command("newMessage")
        /// File → Add Proton Account… (spec/08 "Proton sign-in").
        static let addProtonAccount = command("addProtonAccount")
        /// File → Import… (spec/09).
        static let importMail = command("importMail")
        /// File → Save: the key compose window's draft (spec/08 "Saving").
        static let saveDraft = command("saveDraft")
        /// File → Attach Files… (spec/08 "Attachments").
        static let attachFiles = command("attachFiles")
        /// Message → Send (spec/08 "Sending").
        static let sendMessage = command("sendMessage")
        /// Format → Make Plain Text or Make Markdown.
        static let toggleFormat = command("toggleFormat")
        /// View → Message Source (spec/08 "Reader").
        static let messageSource = command("messageSource")
        /// View → Reading View.
        static let readingView = command("readingView")
        /// Edit → Paste as Markdown… (spec/08 "Composer").
        static let pasteAsMarkdown = command("pasteAsMarkdown")
        /// Edit → Insert Image….
        static let insertImage = command("insertImage")
        /// Edit → Edit in External Editor.
        static let editInExternalEditor = command("editInExternalEditor")
        /// Format → Bold.
        static let bold = command("bold")
        /// Format → Italic.
        static let italic = command("italic")
        /// Message → Open in New Window (spec/08 "Message window").
        static let openMessage = command("openMessage")
        /// Message → Reply (spec/08 "Cross-account check").
        static let reply = command("reply")
        /// Message → Reply All.
        static let replyAll = command("replyAll")
        /// Message → Forward.
        static let forward = command("forward")
        /// Message → Mark as Read or Unread (spec/08 "Message list").
        static let toggleRead = command("toggleRead")
        /// Message → Star or Unstar.
        static let toggleStar = command("toggleStar")
        /// Message → Archive (spec/08 "Keyboard filing").
        static let archive = command("archive")
        /// Message → Move to Trash.
        static let trash = command("trash")
        /// Message → Move to….
        static let move = command("move")
        /// Message → Add Label….
        static let addLabel = command("addLabel")
        /// Message → Remove Label….
        static let removeLabel = command("removeLabel")
        /// Window → Activity.
        static let showActivity = command("showActivity")
        /// Debug → Fill 500,000 Synthetic Rows (spec/08 "Debug menu").
        static let fillSynthetic = command("debug.fillSynthetic")
        /// Debug → List Prototype.
        static let listPrototype = command("debug.listPrototype")
        /// Debug → List Implementation → one implementation, named by
        /// its stored value.
        static func listImplementation(_ raw: String) -> NSUserInterfaceItemIdentifier {
            command("debug.listImplementation.\(raw)")
        }
        /// Debug → Run List Benchmark.
        static let listBenchmark = command("debug.runListBenchmark")
        /// Debug → Run Store Benchmark.
        static let storeBenchmark = command("debug.runStoreBenchmark")
        /// Debug → Run Search Benchmark.
        static let searchBenchmark = command("debug.runSearchBenchmark")
        /// The three benchmarks, disabled while a run is in flight.
        static let benchmarks: Set<NSUserInterfaceItemIdentifier> = [
            listBenchmark, storeBenchmark, searchBenchmark,
        ]
        /// Debug → Composer Probe.
        static let composerProbe = command("debug.composerProbe")
        /// Debug → Record Interaction Latency, checked while it is on.
        static let recordLatency = command("debug.recordLatency")
        /// Debug → Latency Report….
        static let latencyReport = command("debug.latencyReport")
        /// Debug → Clear Latency Log.
        static let clearLatencyLog = command("debug.clearLatencyLog")
    }

    /// - Parameters:
    ///   - onFillSynthetic: run the 500k synthetic fill and rebuild the
    ///     open prototype models.
    ///   - onShowPrototype: open the prototype window or bring it
    ///     forward.
    ///   - onImplementationChanged: persist-and-swap the list
    ///     implementation — the builder stores the choice, the app
    ///     swaps any open window's list.
    ///   - onRunBenchmark: run part B2b's list benchmark.
    ///   - onRunSearchBenchmark: time Search's last keystroke to first
    ///     rows over the open archive (plan M1.6q2).
    ///   - isBenchmarkEnabled: whether Run List Benchmark is live —
    ///     false while a run is in flight.
    ///   - onShowComposerProbe: open the Composer Probe window or
    ///     bring it forward (plan M0.11).
    ///   - onImport: File → Import… (spec/09 "In the app").
    ///   - onAddProtonAccount: File → Add Proton Account… (plan
    ///     M2.14b; the Bridge sheet is no longer offered, spec/08).
    ///   - onReply: Message → Reply, from the message list's single
    ///     selection (spec/08 "Cross-account check").
    ///   - onReplyAll: Message → Reply All.
    ///   - onForward: Message → Forward.
    ///   - isReplyEnabled: whether exactly one occurrence is selected.
    init(
        onFillSynthetic: @escaping @MainActor () -> Void,
        onShowPrototype: @escaping @MainActor () -> Void,
        onImplementationChanged: @escaping @MainActor (PrototypeImplementation) -> Void,
        onRunBenchmark: @escaping @MainActor () -> Void,
        onRunStoreBenchmark: @escaping @MainActor () -> Void,
        onRunSearchBenchmark: @escaping @MainActor () -> Void,
        isBenchmarkEnabled: @escaping @MainActor () -> Bool,
        onShowComposerProbe: @escaping @MainActor () -> Void,
        onImport: @escaping @MainActor () -> Void,
        onAddProtonAccount: @escaping @MainActor () -> Void = {},
        onSettings: @escaping @MainActor () -> Void = {},
        onShowActivity: @escaping @MainActor () -> Void = {},
        isWriteEnabled: @escaping @MainActor () -> Bool = { true },
        onNewMessage: @escaping @MainActor () -> Void = {},
        isComposeEnabled: @escaping @MainActor () -> Bool = { true },
        onReply: @escaping @MainActor () -> Void = {},
        onReplyAll: @escaping @MainActor () -> Void = {},
        onForward: @escaping @MainActor () -> Void = {},
        isReplyEnabled: @escaping @MainActor () -> Bool = { false },
        latency: LatencyRecorder = .shared
    ) {
        self.latency = latency
        self.onFillSynthetic = onFillSynthetic
        self.onShowPrototype = onShowPrototype
        self.onImplementationChanged = onImplementationChanged
        self.onRunBenchmark = onRunBenchmark
        self.onRunStoreBenchmark = onRunStoreBenchmark
        self.onRunSearchBenchmark = onRunSearchBenchmark
        self.isBenchmarkEnabled = isBenchmarkEnabled
        self.onShowComposerProbe = onShowComposerProbe
        self.onImport = onImport
        self.onNewMessage = onNewMessage
        self.isComposeEnabled = isComposeEnabled
        self.onReply = onReply
        self.onReplyAll = onReplyAll
        self.onForward = onForward
        self.isReplyEnabled = isReplyEnabled
        self.onAddProtonAccount = onAddProtonAccount
        self.onSettings = onSettings
        self.onShowActivity = onShowActivity
        self.isWriteEnabled = isWriteEnabled
        super.init()
    }

    /// Build the whole menu bar and register the Window menu with the
    /// app.
    func makeMainMenu() -> NSMenu {
        let menu = MainMenuBar()
        menu.addItem(makeAppMenu())
        menu.addItem(makeFileMenu())
        menu.addItem(makeEditMenu())
        menu.addItem(makeViewMenu())
        menu.addItem(makeMailboxMenu())
        menu.addItem(makeMessageMenu())
        menu.addItem(makeFormatMenu())
        menu.addItem(makeWindowMenu())
        menu.addItem(makeDebugMenu())
        menu.addItem(makeHelpMenu())
        return menu
    }

    /// The app menu: About, Hide, Quit (spec/08 "Debug menu (pilot)").
    private func makeAppMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Sweetbriar")
        menu.addItem(Self.item(
            "About Sweetbriar",
            action: #selector(NSApplication.orderFrontStandardAboutPanel(_:))
        ))
        menu.addItem(.separator())
        let settings = Self.item(
            "Settings…", action: #selector(showSettings(_:)), key: ",", id: ItemID.settings)
        settings.target = self
        menu.addItem(settings)
        menu.addItem(.separator())
        let services = NSMenuItem(title: "Services", action: nil, keyEquivalent: "")
        services.submenu = NSMenu(title: "Services")
        NSApplication.shared.servicesMenu = services.submenu
        menu.addItem(services)
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Hide Sweetbriar",
            action: #selector(NSApplication.hide(_:)),
            key: "h"
        ))
        menu.addItem(Self.item(
            "Hide Others",
            action: #selector(NSApplication.hideOtherApplications(_:)),
            key: "h",
            modifiers: [.option, .command]
        ))
        menu.addItem(Self.item(
            "Show All",
            action: #selector(NSApplication.unhideAllApplications(_:))
        ))
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Quit Sweetbriar",
            action: #selector(NSApplication.terminate(_:)),
            key: "q"
        ))
        item.submenu = menu
        return item
    }

    @objc private func showSettings(_ sender: Any?) { onSettings() }

    /// Window → Activity (spec/08 "Activity / account health").
    @objc private func showActivity(_ sender: NSMenuItem) { onShowActivity() }

    /// File: local composition, the key window's Close and the draft's
    /// Save and Attach Files…, then account setup and import (spec/08–09).
    private func makeFileMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "File")
        menu.addItem(Self.item(
            "New Message", action: #selector(newMessage(_:)), key: "n", target: self,
            id: ItemID.newMessage))
        menu.addItem(.separator())
        menu.addItem(Self.item("Close", action: #selector(NSWindow.performClose(_:)), key: "w"))
        // Nil-target: the key compose window answers (spec/08 "Saving").
        menu.addItem(Self.item(
            "Save", action: #selector(ComposeCommands.saveDraft(_:)), key: "s", id: ItemID.saveDraft))
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Attach Files…", action: #selector(ComposeCommands.attachFiles(_:)), key: "A",
            id: ItemID.attachFiles))
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Add Proton Account…",
            action: #selector(addProtonAccount(_:)),
            key: "",
            target: self,
            id: ItemID.addProtonAccount
        ))
        menu.addItem(Self.item(
            "Import…",
            action: #selector(importMail(_:)),
            key: "I",
            target: self,
            id: ItemID.importMail
        ))
        item.submenu = menu
        return item
    }

    /// Native editing actions follow focus, including text expanders'
    /// Paste shortcut and the composer's undo manager.
    private func makeEditMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Edit")
        menu.addItem(Self.item("Undo", action: #selector(EditUndoCommands.undo(_:)), key: "z"))
        menu.addItem(Self.item(
            "Redo",
            action: #selector(EditUndoCommands.redo(_:)),
            key: "Z"
        ))
        menu.addItem(.separator())
        menu.addItem(Self.item("Cut", action: #selector(NSText.cut(_:)), key: "x"))
        menu.addItem(Self.item("Copy", action: #selector(NSText.copy(_:)), key: "c"))
        menu.addItem(Self.item("Paste", action: #selector(NSText.paste(_:)), key: "v"))
        menu.addItem(Self.item(
            "Paste as Markdown…",
            action: #selector(ComposeWindowController.pasteAsMarkdown(_:)),
            key: "v",
            modifiers: [.option, .command],
            id: ItemID.pasteAsMarkdown
        ))
        menu.addItem(Self.item("Delete", action: #selector(NSText.delete(_:))))
        menu.addItem(Self.item(
            "Select All",
            action: #selector(NSText.selectAll(_:)),
            key: "a"
        ))
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Insert Image…",
            action: #selector(ComposeWindowController.insertImage(_:)),
            key: "",
            id: ItemID.insertImage
        ))
        menu.addItem(Self.item(
            "Edit in External Editor",
            action: #selector(ComposeWindowController.editInExternalEditor(_:)),
            key: "E",
            id: ItemID.editInExternalEditor
        ))
        menu.addItem(.separator())
        for submenu in StandardTextMenus.editSubmenus() { menu.addItem(submenu) }
        item.submenu = menu
        return item
    }

    /// View: the window's sidebar and toolbar, the reader's renditions
    /// (spec/08 "Reader") and full screen. AppKit's own items retitle
    /// themselves (Show or Hide Sidebar, Show or Hide Toolbar).
    private func makeViewMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "View")
        menu.addItem(Self.item(
            "Show Sidebar", action: #selector(NSSplitViewController.toggleSidebar(_:)), key: "s",
            modifiers: [.control, .command]))
        menu.addItem(.separator())
        // Nil-target: the key main or message window's reader answers.
        menu.addItem(Self.item(
            "Message Source", action: #selector(ReaderCommands.toggleMessageSource(_:)), key: "u",
            modifiers: [.option, .command], id: ItemID.messageSource))
        menu.addItem(Self.item(
            "Reading View", action: #selector(ReaderCommands.toggleReadingView(_:)), key: "r",
            modifiers: [.option, .command], id: ItemID.readingView))
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Show Toolbar", action: #selector(NSWindow.toggleToolbarShown(_:)), key: "t",
            modifiers: [.option, .command]))
        menu.addItem(Self.item(
            "Customize Toolbar…", action: #selector(NSWindow.runToolbarCustomizationPalette(_:))))
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Enter Full Screen", action: #selector(NSWindow.toggleFullScreen(_:)), key: "f",
            modifiers: [.control, .command]))
        item.submenu = menu
        return item
    }

    /// Mailbox: go to the unified mailboxes with ⌘1–⌘4, as Mail's
    /// favorites do; the key main window's sidebar answers.
    private func makeMailboxMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Mailbox")
        for (index, destination) in MailboxDestination.allCases.enumerated() {
            let goTo = Self.item(
                destination.title, action: #selector(MailboxCommands.goToMailbox(_:)),
                key: "\(index + 1)", id: destination.identifier)
            goTo.representedObject = destination.entryID
            menu.addItem(goTo)
        }
        item.submenu = menu
        return item
    }

    /// Help: AppKit adds its menu search field to the registered menu.
    private func makeHelpMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Help")
        NSApplication.shared.helpMenu = menu
        item.submenu = menu
        return item
    }

    /// Message: Open in New Window, then Reply, Reply All and Forward,
    /// each from the message list's single selection or the key message
    /// window's message (spec/08 "Cross-account check", "Message
    /// window").
    private func makeMessageMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Message")
        // ⇧⌘D, nil-target: the key compose window sends, as its Send
        // button does (spec/08 "Sending").
        menu.addItem(Self.item(
            "Send", action: #selector(ComposeCommands.sendMessage(_:)), key: "D", id: ItemID.sendMessage))
        menu.addItem(.separator())
        // ⌘O, nil-target: the main window opens its list's single
        // selection in its own window (spec/08 "Message window").
        let open = Self.item(
            "Open in New Window", action: #selector(MessageWindowCommands.openMessage(_:)),
            key: "o")
        open.identifier = ItemID.openMessage
        menu.addItem(open)
        menu.addItem(.separator())
        let reply = Self.item("Reply", action: #selector(reply(_:)), key: "r", target: self)
        reply.identifier = ItemID.reply
        menu.addItem(reply)
        let replyAll = Self.item(
            "Reply All", action: #selector(replyAll(_:)), key: "R", target: self)
        replyAll.identifier = ItemID.replyAll
        menu.addItem(replyAll)
        let forward = Self.item(
            "Forward", action: #selector(forward(_:)), key: "F", target: self)
        forward.identifier = ItemID.forward
        menu.addItem(forward)
        menu.addItem(.separator())
        // ⇧⌘U and ⇧⌘L (an uppercase key implies Shift), nil-target: the
        // main window acts on its list's selection and retitles both for
        // what they would do (spec/08 "Message list").
        let read = Self.item(
            "Mark as Read", action: #selector(MessageFlagCommands.toggleRead(_:)), key: "U")
        read.identifier = ItemID.toggleRead
        menu.addItem(read)
        let star = Self.item(
            "Star", action: #selector(MessageFlagCommands.toggleStar(_:)), key: "L")
        star.identifier = ItemID.toggleStar
        menu.addItem(star)
        menu.addItem(.separator())
        addFilingItems(to: menu)
        item.submenu = menu
        return item
    }

    /// Archive (⌃⌘A), Move to Trash (⌘⌫), Move to… (⇧⌘M) and the two
    /// label commands, nil-target like the flag commands (spec/08
    /// "Keyboard filing").
    private func addFilingItems(to menu: NSMenu) {
        let archive = Self.item(
            "Archive", action: #selector(MessageFilingCommands.archiveMessages(_:)),
            key: "a", modifiers: [.control, .command])
        archive.identifier = ItemID.archive
        menu.addItem(archive)
        let trash = Self.item(
            "Move to Trash", action: #selector(MessageFilingCommands.trashMessages(_:)),
            key: "\u{8}")
        trash.identifier = ItemID.trash
        menu.addItem(trash)
        let move = Self.item(
            "Move to…", action: #selector(MessageFilingCommands.moveMessages(_:)), key: "M")
        move.identifier = ItemID.move
        menu.addItem(move)
        let addLabel = Self.item(
            "Add Label…", action: #selector(MessageFilingCommands.addLabel(_:)))
        addLabel.identifier = ItemID.addLabel
        menu.addItem(addLabel)
        let removeLabel = Self.item(
            "Remove Label…", action: #selector(MessageFilingCommands.removeLabel(_:)))
        removeLabel.identifier = ItemID.removeLabel
        menu.addItem(removeLabel)
    }

    /// Format: Bold and Italic — nil-target items that walk the
    /// responder chain, so the composer editor's Markdown commands
    /// get them (spec/08 "Composer").
    private func makeFormatMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Format")
        // ⇧⌘T, as Mail's Make Plain Text: the key compose window switches
        // its draft's format and retitles the item for the other one.
        menu.addItem(Self.item(
            "Make Plain Text", action: #selector(ComposeCommands.toggleComposeFormat(_:)), key: "T",
            id: ItemID.toggleFormat))
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Bold",
            action: #selector(ComposerTextView.toggleBoldMarkdown(_:)),
            key: "b",
            id: ItemID.bold
        ))
        menu.addItem(Self.item(
            "Italic",
            action: #selector(ComposerTextView.toggleItalicMarkdown(_:)),
            key: "i",
            id: ItemID.italic
        ))
        item.submenu = menu
        return item
    }

    /// Window: Minimize, Zoom, Activity and Bring All to Front;
    /// registered as the app's `windowsMenu`, so AppKit adds its tiling
    /// items and the window list. Close lives in File.
    private func makeWindowMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Window")
        menu.addItem(Self.item(
            "Minimize",
            action: #selector(NSWindow.performMiniaturize(_:)),
            key: "m"
        ))
        menu.addItem(Self.item("Zoom", action: #selector(NSWindow.performZoom(_:))))
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Activity",
            action: #selector(showActivity(_:)),
            key: "0",
            modifiers: [.option, .command],
            target: self,
            id: ItemID.showActivity
        ))
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Bring All to Front", action: #selector(NSApplication.arrangeInFront(_:))))
        NSApplication.shared.windowsMenu = menu
        item.submenu = menu
        return item
    }

    /// Debug (spec/08): the synthetic fill, the prototype window, the
    /// implementation radio submenu, and the disabled benchmark stub.
    private func makeDebugMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Debug")
        menu.addItem(Self.item(
            "Fill 500,000 Synthetic Rows",
            action: #selector(fillSyntheticRows(_:)),
            target: self,
            id: ItemID.fillSynthetic
        ))
        menu.addItem(Self.item(
            "List Prototype",
            action: #selector(showListPrototype(_:)),
            target: self,
            id: ItemID.listPrototype
        ))
        menu.addItem(makeImplementationItem())
        menu.addItem(.separator())
        let benchmark = Self.item(
            "Run List Benchmark",
            action: #selector(runListBenchmark(_:)),
            target: self
        )
        benchmark.identifier = ItemID.listBenchmark
        benchmark.toolTip =
            "Run the four list benchmark runs and show the report"
        menu.addItem(benchmark)
        let storeBenchmark = Self.item(
            "Run Store Benchmark",
            action: #selector(runStoreBenchmark(_:)),
            target: self
        )
        storeBenchmark.identifier = ItemID.storeBenchmark
        storeBenchmark.toolTip =
            "Run the list benchmark over the imported mailboxes and show the report"
        menu.addItem(storeBenchmark)
        let searchBenchmark = Self.item(
            "Run Search Benchmark",
            action: #selector(runSearchBenchmark(_:)),
            target: self
        )
        searchBenchmark.identifier = ItemID.searchBenchmark
        searchBenchmark.toolTip =
            "Time Search from the last keystroke to the first rows and show the report"
        menu.addItem(searchBenchmark)
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Composer Probe",
            action: #selector(showComposerProbe(_:)),
            target: self,
            id: ItemID.composerProbe
        ))
        menu.addItem(.separator())
        let record = Self.item(
            "Record Interaction Latency",
            action: #selector(toggleLatencyRecording(_:)),
            target: self
        )
        record.identifier = ItemID.recordLatency
        menu.addItem(record)
        menu.addItem(Self.item(
            "Latency Report…",
            action: #selector(showLatencyReport(_:)),
            target: self,
            id: ItemID.latencyReport
        ))
        menu.addItem(Self.item(
            "Clear Latency Log",
            action: #selector(clearLatencyLog(_:)),
            target: self,
            id: ItemID.clearLatencyLog
        ))
        item.submenu = menu
        return item
    }

    /// The radio-checked AppKit / SwiftUI submenu (spec/08 "List
    /// Implementation").
    private func makeImplementationItem() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "List Implementation")
        for implementation in PrototypeImplementation.allCases {
            let subItem = Self.item(
                implementation.displayName,
                action: #selector(pickImplementation(_:)),
                target: self
            )
            subItem.representedObject = implementation.rawValue
            subItem.identifier = ItemID.listImplementation(implementation.rawValue)
            subItem.state = implementation == .stored ? .on : .off
            menu.addItem(subItem)
        }
        item.submenu = menu
        return item
    }

    /// One item: `target` nil sends the action down the responder
    /// chain (Copy, Quit, Close); the Debug items target the builder.
    private static func item(
        _ title: String,
        action: Selector,
        key: String = "",
        modifiers: NSEvent.ModifierFlags = [.command],
        target: NSObject? = nil,
        id: NSUserInterfaceItemIdentifier? = nil
    ) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        if !key.isEmpty {
            item.keyEquivalentModifierMask = modifiers
        }
        item.target = target
        item.identifier = id
        return item
    }

    @objc
    private func fillSyntheticRows(_ sender: NSMenuItem) {
        onFillSynthetic()
    }

    @objc
    private func addProtonAccount(_ sender: NSMenuItem) {
        onAddProtonAccount()
    }

    @objc private func newMessage(_ sender: NSMenuItem) { onNewMessage() }

    @objc private func reply(_ sender: NSMenuItem) { onReply() }

    @objc private func replyAll(_ sender: NSMenuItem) { onReplyAll() }

    @objc private func forward(_ sender: NSMenuItem) { onForward() }

    @objc
    private func importMail(_ sender: NSMenuItem) {
        onImport()
    }

    @objc
    private func showListPrototype(_ sender: NSMenuItem) {
        onShowPrototype()
    }

    /// A radio pick: persist the choice, refresh both items' checks,
    /// then let the app swap the open window's list.
    @objc
    private func pickImplementation(_ sender: NSMenuItem) {
        guard let raw = sender.representedObject as? String,
              let implementation = PrototypeImplementation(rawValue: raw)
        else {
            return
        }
        PrototypeImplementation.stored = implementation
        onImplementationChanged(implementation)
    }

    /// Debug → Run List Benchmark (spec/08, plan M0.10).
    @objc
    private func runListBenchmark(_ sender: NSMenuItem) {
        onRunBenchmark()
    }

    @objc
    private func runStoreBenchmark(_ sender: NSMenuItem) {
        onRunStoreBenchmark()
    }

    @objc
    private func runSearchBenchmark(_ sender: NSMenuItem) {
        onRunSearchBenchmark()
    }

    /// Debug → Composer Probe (spec/08, plan M0.11).
    @objc
    private func showComposerProbe(_ sender: NSMenuItem) {
        onShowComposerProbe()
    }

    /// Debug → Record Interaction Latency (spec/08 "Latency log").
    @objc
    private func toggleLatencyRecording(_ sender: NSMenuItem) {
        latency.isEnabled.toggle()
    }

    /// Debug → Latency Report…: the report as it stands, with Copy.
    @objc
    private func showLatencyReport(_ sender: NSMenuItem) {
        let window = BenchmarkReportWindowController(title: "Latency Report", text: latency.reportText())
        latencyReport = window
        window.showWindow(nil)
    }

    /// Debug → Clear Latency Log; a failure says so.
    @objc
    private func clearLatencyLog(_ sender: NSMenuItem) {
        guard let error = latency.clear() else { return }
        let alert = NSAlert()
        alert.messageText = "Couldn't clear the latency log"
        alert.informativeText = error.localizedDescription
        alert.runModal()
    }
}

extension MainMenuBuilder: NSMenuItemValidation {
    /// Every Debug item is enabled except the benchmark while a run
    /// is in flight; the implementation pair re-checks itself against
    /// the stored choice each time the menu opens.
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.action == #selector(newMessage(_:)) { return isWriteEnabled() && isComposeEnabled() }
        if let action = menuItem.action,
           [#selector(reply(_:)), #selector(replyAll(_:)), #selector(forward(_:))]
            .contains(action) {
            // A reply or forward creates a draft: the same write and
            // compose admission as New Message, plus one selection.
            return isWriteEnabled() && isComposeEnabled() && isReplyEnabled()
        }
        if let action = menuItem.action,
           [#selector(importMail(_:)), #selector(addProtonAccount(_:)), #selector(fillSyntheticRows(_:))]
            .contains(action) {
            return isWriteEnabled()
        }
        if let identifier = menuItem.identifier, ItemID.benchmarks.contains(identifier) {
            return isBenchmarkEnabled()
        }
        if menuItem.identifier == ItemID.recordLatency {
            menuItem.state = latency.isEnabled ? .on : .off
        }
        if menuItem.action == #selector(pickImplementation(_:)) {
            let raw = menuItem.representedObject as? String
            menuItem.state =
                raw.flatMap(PrototypeImplementation.init) == .stored ? .on : .off
        }
        return true
    }
}

/// The responder chain's undo and redo. AppKit's text views and windows
/// answer `undo:` and `redo:` but declare them nowhere Swift can name,
/// and Xcode 27 refuses a `Selector` spelled out by hand.
@MainActor
@objc protocol EditUndoCommands {
    func undo(_ sender: Any?)
    func redo(_ sender: Any?)
}

/// The key compose window's draft commands, named here so the menu's
/// nil-target items reach it down the responder chain (spec/08
/// "Composer").
@MainActor
@objc protocol ComposeCommands {
    func saveDraft(_ sender: Any?)
    func attachFiles(_ sender: Any?)
    func sendMessage(_ sender: Any?)
    func toggleComposeFormat(_ sender: Any?)
}

/// The shown message's renditions, answered by the key main or message
/// window for its reader (spec/08 "Reader").
@MainActor
@objc protocol ReaderCommands {
    func toggleMessageSource(_ sender: Any?)
    func toggleReadingView(_ sender: Any?)
}

/// Mailbox → the unified mailboxes, answered by the key main window's
/// sidebar; the item's represented object is the entry's id.
@MainActor
@objc protocol MailboxCommands {
    func goToMailbox(_ sender: Any?)
}

/// The unified mailboxes the Mailbox menu goes to with ⌘1–⌘4.
@MainActor
enum MailboxDestination: CaseIterable {
    case inbox
    case flagged
    case drafts
    case outBox

    var title: String {
        switch self {
        case .inbox: "Inbox"
        case .flagged: "Flagged"
        case .drafts: "Drafts (local)"
        case .outBox: "Out Box"
        }
    }

    /// The sidebar entry it chooses (spec/08 "Sidebar").
    var entryID: String {
        switch self {
        case .inbox: SidebarModel.unifiedInboxID
        case .flagged: SidebarModel.unifiedFlaggedID
        case .drafts: SidebarModel.unifiedDraftsID
        case .outBox: SidebarModel.unifiedOutBoxID
        }
    }

    var identifier: NSUserInterfaceItemIdentifier {
        switch self {
        case .inbox: MainMenuBuilder.ItemID.command("goToInbox")
        case .flagged: MainMenuBuilder.ItemID.command("goToFlagged")
        case .drafts: MainMenuBuilder.ItemID.command("goToDrafts")
        case .outBox: MainMenuBuilder.ItemID.command("goToOutBox")
        }
    }
}

/// The Edit menu's standard text submenus. Every item is AppKit's own
/// action, so it reaches the focused text view and is disabled where
/// nothing answers it (the HTML reader, the message list).
@MainActor
enum StandardTextMenus {
    static func editSubmenus() -> [NSMenuItem] {
        [find(), spelling(), substitutions(), transformations(), speech()]
    }

    private static func submenu(_ title: String, _ items: [NSMenuItem]) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: nil, keyEquivalent: "")
        let menu = NSMenu(title: title)
        for child in items { menu.addItem(child) }
        item.submenu = menu
        return item
    }

    private static func item(
        _ title: String, _ action: Selector, _ key: String = "",
        _ modifiers: NSEvent.ModifierFlags = [.command], tag: Int = 0
    ) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        if !key.isEmpty { item.keyEquivalentModifierMask = modifiers }
        item.tag = tag
        return item
    }

    /// Find, through the focused text view's find bar.
    private static func find() -> NSMenuItem {
        let action = #selector(NSTextView.performFindPanelAction(_:))
        return submenu("Find", [
            item("Find…", action, "f", tag: Int(NSFindPanelAction.showFindPanel.rawValue)),
            item("Find and Replace…", action,
                 tag: Int(NSTextFinder.Action.showReplaceInterface.rawValue)),
            item("Find Next", action, "g", tag: Int(NSFindPanelAction.next.rawValue)),
            item("Find Previous", action, "G", tag: Int(NSFindPanelAction.previous.rawValue)),
            item("Use Selection for Find", action, "e",
                 tag: Int(NSFindPanelAction.setFindString.rawValue)),
            item("Jump to Selection", #selector(NSResponder.centerSelectionInVisibleArea(_:)), "j"),
        ])
    }

    private static func spelling() -> NSMenuItem {
        submenu("Spelling and Grammar", [
            item("Show Spelling and Grammar", #selector(NSText.showGuessPanel(_:)), ":"),
            item("Check Document Now", #selector(NSText.checkSpelling(_:)), ";"),
            .separator(),
            item("Check Spelling While Typing",
                 #selector(NSTextView.toggleContinuousSpellChecking(_:))),
            item("Check Grammar With Spelling", #selector(NSTextView.toggleGrammarChecking(_:))),
            item("Correct Spelling Automatically",
                 #selector(NSTextView.toggleAutomaticSpellingCorrection(_:))),
        ])
    }

    private static func substitutions() -> NSMenuItem {
        submenu("Substitutions", [
            item("Show Substitutions", #selector(NSTextView.orderFrontSubstitutionsPanel(_:))),
            .separator(),
            item("Smart Copy/Paste", #selector(NSTextView.toggleSmartInsertDelete(_:))),
            item("Smart Quotes", #selector(NSTextView.toggleAutomaticQuoteSubstitution(_:))),
            item("Smart Dashes", #selector(NSTextView.toggleAutomaticDashSubstitution(_:))),
            item("Smart Links", #selector(NSTextView.toggleAutomaticLinkDetection(_:))),
            item("Data Detectors", #selector(NSTextView.toggleAutomaticDataDetection(_:))),
            item("Text Replacement", #selector(NSTextView.toggleAutomaticTextReplacement(_:))),
        ])
    }

    private static func transformations() -> NSMenuItem {
        submenu("Transformations", [
            item("Make Upper Case", #selector(NSResponder.uppercaseWord(_:))),
            item("Make Lower Case", #selector(NSResponder.lowercaseWord(_:))),
            item("Capitalize", #selector(NSResponder.capitalizeWord(_:))),
        ])
    }

    private static func speech() -> NSMenuItem {
        submenu("Speech", [
            item("Start Speaking", #selector(NSTextView.startSpeaking(_:))),
            item("Stop Speaking", #selector(NSTextView.stopSpeaking(_:))),
        ])
    }
}

/// The main window's Open in New Window, named here so the menu's
/// nil-target item can reach it down the responder chain (spec/08
/// "Message window").
@MainActor
@objc protocol MessageWindowCommands {
    func openMessage(_ sender: Any?)
}

/// The main window's read and star commands, named here so the menu's
/// nil-target items can reach them down the responder chain.
@MainActor
@objc protocol MessageFlagCommands {
    func toggleRead(_ sender: Any?)
    func toggleStar(_ sender: Any?)
}

/// The main window's filing commands (spec/08 "Keyboard filing").
@MainActor
@objc protocol MessageFilingCommands {
    func archiveMessages(_ sender: Any?)
    func trashMessages(_ sender: Any?)
    func moveMessages(_ sender: Any?)
    func addLabel(_ sender: Any?)
    func removeLabel(_ sender: Any?)
}
