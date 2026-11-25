// The main window's toolbar (spec/08 "Toolbar"), laid out as Mail's:
// the sidebar button, then New Message and the message commands in
// groups after the title, and the search field at the trailing end.
//
//   [◧] │ Inbox            [✎] [↩ ↩↩ ↪] [▣ 🗑 ▭] [🏷▾] [✉ ☆] [🔍 Search ]
//        Local Archive · 240 messages
//
// Every item is a command the menus also carry, under the menu's
// identifier; groups validate each of their items, so a disabled
// command is never clickable.

import AppKit

@MainActor
enum MainToolbar {
    typealias ID = NSToolbarItem.Identifier
    private typealias Command = MainMenuBuilder.ItemID

    static let newMessage = ID(Command.newMessage.rawValue)
    static let replyGroup = ID("ch.lkmc.Sweetbriar.toolbar.reply")
    static let filingGroup = ID("ch.lkmc.Sweetbriar.toolbar.filing")
    static let labels = ID("ch.lkmc.Sweetbriar.toolbar.labels")
    static let flagGroup = ID("ch.lkmc.Sweetbriar.toolbar.flags")
    static let search = ID("ch.lkmc.Sweetbriar.toolbar.search")
    static let searchFieldWidth: CGFloat = 220

    static var defaultItems: [ID] {
        [
            .toggleSidebar, .sidebarTrackingSeparator, .flexibleSpace, newMessage, replyGroup,
            filingGroup, labels, flagGroup, search,
        ]
    }

    static var allowedItems: [ID] {
        defaultItems + [.space]
    }

    /// One command inside a group, or on its own.
    private struct Spec {
        let id: NSUserInterfaceItemIdentifier
        let title: String
        let symbol: String
        let action: Selector
    }

    private static let reply = Spec(
        id: Command.reply, title: "Reply", symbol: "arrowshape.turn.up.left",
        action: #selector(MainMenuBuilder.reply(_:)))
    private static let replyAll = Spec(
        id: Command.replyAll, title: "Reply All", symbol: "arrowshape.turn.up.left.2",
        action: #selector(MainMenuBuilder.replyAll(_:)))
    private static let forward = Spec(
        id: Command.forward, title: "Forward", symbol: "arrowshape.turn.up.right",
        action: #selector(MainMenuBuilder.forward(_:)))
    private static let archive = Spec(
        id: Command.archive, title: "Archive", symbol: "archivebox",
        action: #selector(MessageFilingCommands.archiveMessages(_:)))
    private static let trash = Spec(
        id: Command.trash, title: "Move to Trash", symbol: "trash",
        action: #selector(MessageFilingCommands.trashMessages(_:)))
    private static let move = Spec(
        id: Command.move, title: "Move to…", symbol: "folder",
        action: #selector(MessageFilingCommands.moveMessages(_:)))
    private static let read = Spec(
        id: Command.toggleRead, title: "Mark as Read", symbol: "envelope.open",
        action: #selector(MessageFlagCommands.toggleRead(_:)))
    private static let star = Spec(
        id: Command.toggleStar, title: "Star", symbol: "star",
        action: #selector(MessageFlagCommands.toggleStar(_:)))

    /// The item for `identifier`, or nil for AppKit's own and the
    /// search item, which the window makes. Compose commands target the
    /// menu builder; the others go down the responder chain.
    static func item(_ identifier: ID, composeTarget: AnyObject?) -> NSToolbarItem? {
        switch identifier {
        case newMessage:
            let item = NSToolbarItem(itemIdentifier: identifier)
            configure(item, title: "New Message", symbol: "square.and.pencil")
            item.action = #selector(MainMenuBuilder.newMessage(_:))
            item.target = composeTarget
            return item
        case replyGroup:
            return group(identifier, label: "Reply", [reply, replyAll, forward], target: composeTarget)
        case filingGroup:
            return group(identifier, label: "Filing", [archive, trash, move], target: nil)
        case labels:
            return labelsItem(identifier)
        case flagGroup:
            return group(identifier, label: "Read and Star", [read, star], target: nil)
        default:
            return nil
        }
    }

    /// The symbol for a flag command's current title, so the button
    /// shows what a click does.
    static func flagSymbol(for title: String) -> String? {
        switch title {
        case "Mark as Read": "envelope.open"
        case "Mark as Unread": "envelope.badge"
        case "Star": "star"
        case "Unstar": "star.slash"
        default: nil
        }
    }

    private static func configure(_ item: NSToolbarItem, title: String, symbol: String) {
        item.label = title
        item.paletteLabel = title
        item.toolTip = title
        item.image = NSImage(systemSymbolName: symbol, accessibilityDescription: title)
        item.autovalidates = true
    }

    private static func group(
        _ identifier: ID, label: String, _ specs: [Spec], target: AnyObject?
    ) -> NSToolbarItemGroup {
        let group = ValidatedToolbarItemGroup(itemIdentifier: identifier)
        group.label = label
        group.paletteLabel = label
        group.subitems = specs.map { spec in
            let item = NSToolbarItem(itemIdentifier: ID(spec.id.rawValue))
            configure(item, title: spec.title, symbol: spec.symbol)
            item.action = spec.action
            item.target = target
            return item
        }
        return group
    }

    /// Add Label… and Remove Label… behind one menu button (spec/08
    /// "Keyboard filing": the toolbar carries both).
    private static func labelsItem(_ identifier: ID) -> NSToolbarItem {
        let item = ValidatedMenuToolbarItem(itemIdentifier: identifier)
        configure(item, title: "Labels", symbol: "tag")
        let menu = NSMenu(title: "Labels")
        for (title, action, id) in [
            ("Add Label…", #selector(MessageFilingCommands.addLabel(_:)), Command.addLabel),
            ("Remove Label…", #selector(MessageFilingCommands.removeLabel(_:)), Command.removeLabel),
        ] {
            let entry = NSMenuItem(title: title, action: action, keyEquivalent: "")
            entry.identifier = id
            menu.addItem(entry)
        }
        item.menu = menu
        item.showsIndicator = true
        return item
    }

    /// Whether `action` sent from `item` would reach a target that
    /// admits it now, asking that target as AppKit's autovalidation does.
    static func isEnabled(_ action: Selector, target: AnyObject?, item: NSToolbarItem) -> Bool {
        guard let resolved = NSApp.target(forAction: action, to: target, from: item) else {
            return false
        }
        if let validator = resolved as? NSToolbarItemValidation {
            return validator.validateToolbarItem(item)
        }
        return true
    }
}

/// A group whose items validate one by one: AppKit leaves a group's
/// subitems as they are, so a disabled command would stay clickable.
@MainActor
final class ValidatedToolbarItemGroup: NSToolbarItemGroup {
    override func validate() {
        for item in subitems {
            guard let action = item.action else { continue }
            item.isEnabled = MainToolbar.isEnabled(action, target: item.target, item: item)
        }
        isEnabled = subitems.contains(where: \.isEnabled)
    }
}

/// The Labels menu button: enabled while either of its commands is.
@MainActor
final class ValidatedMenuToolbarItem: NSMenuToolbarItem {
    override func validate() {
        isEnabled = menu.items.contains { entry in
            guard let action = entry.action,
                let target = NSApp.target(forAction: action, to: nil, from: entry)
            else { return false }
            return (target as? NSMenuItemValidation)?.validateMenuItem(entry) ?? true
        }
    }
}
