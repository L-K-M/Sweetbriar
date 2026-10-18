// The main menu, built in code — the app has no nib (spec/08 "Debug
// menu (pilot)"). The Debug menu ships in every build so the owner's
// checklist runs on the CI artifact: the synthetic fill, the list
// prototype, the implementation toggle and the benchmark stub part
// B2b wires.

import AppKit

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
    private let isBenchmarkEnabled: @MainActor () -> Bool
    private let onShowComposerProbe: @MainActor () -> Void
    private let onImport: @MainActor () -> Void

    /// `NSMenuItem.identifier`s that `validateMenuItem` treats
    /// specially.
    private enum ItemID {
        /// Both "List Implementation" submenu items — the radio pair.
        static let implementation = NSUserInterfaceItemIdentifier(
            "debug.listImplementation"
        )
        /// "Run List Benchmark" — disabled while a run is in flight.
        static let benchmark = NSUserInterfaceItemIdentifier(
            "debug.runListBenchmark"
        )
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
    ///   - isBenchmarkEnabled: whether Run List Benchmark is live —
    ///     false while a run is in flight.
    ///   - onShowComposerProbe: open the Composer Probe window or
    ///     bring it forward (plan M0.11).
    ///   - onImport: File → Import… (spec/09 "In the app").
    init(
        onFillSynthetic: @escaping @MainActor () -> Void,
        onShowPrototype: @escaping @MainActor () -> Void,
        onImplementationChanged: @escaping @MainActor (PrototypeImplementation) -> Void,
        onRunBenchmark: @escaping @MainActor () -> Void,
        isBenchmarkEnabled: @escaping @MainActor () -> Bool,
        onShowComposerProbe: @escaping @MainActor () -> Void,
        onImport: @escaping @MainActor () -> Void
    ) {
        self.onFillSynthetic = onFillSynthetic
        self.onShowPrototype = onShowPrototype
        self.onImplementationChanged = onImplementationChanged
        self.onRunBenchmark = onRunBenchmark
        self.isBenchmarkEnabled = isBenchmarkEnabled
        self.onShowComposerProbe = onShowComposerProbe
        self.onImport = onImport
        super.init()
    }

    /// Build the whole menu bar and register the Window menu with the
    /// app.
    func makeMainMenu() -> NSMenu {
        let menu = NSMenu()
        menu.addItem(makeAppMenu())
        menu.addItem(makeFileMenu())
        menu.addItem(makeEditMenu())
        menu.addItem(makeFormatMenu())
        menu.addItem(makeWindowMenu())
        menu.addItem(makeDebugMenu())
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

    /// File: Import… (spec/09 "In the app").
    private func makeFileMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "File")
        menu.addItem(Self.item(
            "Import…",
            action: #selector(importMail(_:)),
            key: "I",
            target: self
        ))
        item.submenu = menu
        return item
    }

    /// Native editing actions follow focus, including text expanders'
    /// Paste shortcut and the composer's undo manager.
    private func makeEditMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Edit")
        menu.addItem(Self.item("Undo", action: Selector(("undo:")), key: "z"))
        menu.addItem(Self.item(
            "Redo",
            action: Selector(("redo:")),
            key: "Z"
        ))
        menu.addItem(.separator())
        menu.addItem(Self.item("Cut", action: #selector(NSText.cut(_:)), key: "x"))
        menu.addItem(Self.item("Copy", action: #selector(NSText.copy(_:)), key: "c"))
        menu.addItem(Self.item("Paste", action: #selector(NSText.paste(_:)), key: "v"))
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Select All",
            action: #selector(NSText.selectAll(_:)),
            key: "a"
        ))
        item.submenu = menu
        return item
    }

    /// Format: Bold and Italic — nil-target items that walk the
    /// responder chain, so the composer editor's Markdown commands
    /// get them (spec/08 "Composer").
    private func makeFormatMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Format")
        menu.addItem(Self.item(
            "Bold",
            action: #selector(ComposerTextView.toggleBoldMarkdown(_:)),
            key: "b"
        ))
        menu.addItem(Self.item(
            "Italic",
            action: #selector(ComposerTextView.toggleItalicMarkdown(_:)),
            key: "i"
        ))
        item.submenu = menu
        return item
    }

    /// Window: Minimize and Close; registered as the app's
    /// `windowsMenu` so window-management items join it.
    private func makeWindowMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Window")
        menu.addItem(Self.item(
            "Minimize",
            action: #selector(NSWindow.performMiniaturize(_:)),
            key: "m"
        ))
        menu.addItem(Self.item(
            "Close",
            action: #selector(NSWindow.performClose(_:)),
            key: "w"
        ))
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
            target: self
        ))
        menu.addItem(Self.item(
            "List Prototype",
            action: #selector(showListPrototype(_:)),
            target: self
        ))
        menu.addItem(makeImplementationItem())
        menu.addItem(.separator())
        let benchmark = Self.item(
            "Run List Benchmark",
            action: #selector(runListBenchmark(_:)),
            target: self
        )
        benchmark.identifier = ItemID.benchmark
        benchmark.toolTip =
            "Run the four list benchmark runs and show the report"
        menu.addItem(benchmark)
        menu.addItem(.separator())
        menu.addItem(Self.item(
            "Composer Probe",
            action: #selector(showComposerProbe(_:)),
            target: self
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
            subItem.identifier = ItemID.implementation
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
        target: NSObject? = nil
    ) -> NSMenuItem {
        let item = NSMenuItem(title: title, action: action, keyEquivalent: key)
        if !key.isEmpty {
            item.keyEquivalentModifierMask = modifiers
        }
        item.target = target
        return item
    }

    @objc
    private func fillSyntheticRows(_ sender: NSMenuItem) {
        onFillSynthetic()
    }

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

    /// Debug → Composer Probe (spec/08, plan M0.11).
    @objc
    private func showComposerProbe(_ sender: NSMenuItem) {
        onShowComposerProbe()
    }
}

extension MainMenuBuilder: NSMenuItemValidation {
    /// Every Debug item is enabled except the benchmark while a run
    /// is in flight; the implementation pair re-checks itself against
    /// the stored choice each time the menu opens.
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.identifier == ItemID.benchmark {
            return isBenchmarkEnabled()
        }
        if menuItem.identifier == ItemID.implementation {
            let raw = menuItem.representedObject as? String
            menuItem.state =
                raw.flatMap(PrototypeImplementation.init) == .stored ? .on : .off
        }
        return true
    }
}
