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
                forKey: MainMenuBuilder.implementationDefaultsKey
            ) else {
                return .appkit
            }
            return PrototypeImplementation(rawValue: raw) ?? .appkit
        }
        set {
            UserDefaults.standard.set(
                newValue.rawValue,
                forKey: MainMenuBuilder.implementationDefaultsKey
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
    /// The user-defaults key "List Implementation" persists under
    /// (spec/08 "Debug menu").
    static let implementationDefaultsKey = "debug.listPrototype.implementation"

    /// The fill count "Fill 500,000 Synthetic Rows" requests (plan
    /// M0.10, spec/07 "Debug source").
    static let syntheticFillCount: UInt32 = 500_000

    private let onFillSynthetic: @MainActor () -> Void
    private let onShowPrototype: @MainActor () -> Void
    private let onImplementationChanged: @MainActor (PrototypeImplementation) -> Void

    /// `NSMenuItem.identifier`s that `validateMenuItem` treats
    /// specially.
    private enum ItemID {
        /// Both "List Implementation" submenu items — the radio pair.
        static let implementation = NSUserInterfaceItemIdentifier(
            "debug.listImplementation"
        )
        /// "Run List Benchmark" — disabled until part B2b wires it.
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
    init(
        onFillSynthetic: @escaping @MainActor () -> Void,
        onShowPrototype: @escaping @MainActor () -> Void,
        onImplementationChanged: @escaping @MainActor (PrototypeImplementation) -> Void
    ) {
        self.onFillSynthetic = onFillSynthetic
        self.onShowPrototype = onShowPrototype
        self.onImplementationChanged = onImplementationChanged
        super.init()
    }

    /// Build the whole menu bar and register the Window menu with the
    /// app.
    func makeMainMenu() -> NSMenu {
        let menu = NSMenu()
        menu.addItem(makeAppMenu())
        menu.addItem(makeEditMenu())
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

    /// Edit: Copy and Select All — first-responder actions the
    /// benchmark window and the lists use.
    private func makeEditMenu() -> NSMenuItem {
        let item = NSMenuItem()
        let menu = NSMenu(title: "Edit")
        menu.addItem(Self.item("Copy", action: #selector(NSText.copy(_:)), key: "c"))
        menu.addItem(Self.item(
            "Select All",
            action: #selector(NSText.selectAll(_:)),
            key: "a"
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
        benchmark.toolTip = "Added in part B2b"
        menu.addItem(benchmark)
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

    /// The benchmark stub — disabled; part B2b puts the run behind it.
    @objc
    private func runListBenchmark(_ sender: NSMenuItem) {}
}

extension MainMenuBuilder: NSMenuItemValidation {
    /// Every Debug item is enabled except the benchmark stub; the
    /// implementation pair re-checks itself against the stored choice
    /// each time the menu opens.
    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        if menuItem.identifier == ItemID.benchmark {
            return false
        }
        if menuItem.identifier == ItemID.implementation {
            let raw = menuItem.representedObject as? String
            menuItem.state =
                raw.flatMap(PrototypeImplementation.init) == .stored ? .on : .off
        }
        return true
    }
}
