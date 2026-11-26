// Help → Keyboard Shortcuts…: one window listing the menu bar's
// keyboard commands, grouped as the menus group them. The catalog is
// Foundation-only so tests can check it; the window is a SwiftUI
// island like Settings (spec/08 "Platform").

import AppKit
import SwiftUI

/// One row of the reference: what the command does and its keys.
struct ShortcutCatalogRow: Sendable, Identifiable, Equatable {
    var action: String
    var keys: String

    var id: String { action }
}

/// One menu's group of rows.
struct ShortcutCatalogSection: Sendable, Identifiable, Equatable {
    var title: String
    var rows: [ShortcutCatalogRow]

    var id: String { title }
}

/// The commands a user is most likely to want at a glance. Titles and
/// keys mirror `MainMenu.swift`; a change there should change this
/// table. `keys` are display strings, not `keyEquivalent` values.
enum ShortcutCatalog {
    static let sections: [ShortcutCatalogSection] = [
        ShortcutCatalogSection(title: "File", rows: [
            ShortcutCatalogRow(action: "New Message", keys: "⌘N"),
            ShortcutCatalogRow(action: "Close", keys: "⌘W"),
            ShortcutCatalogRow(action: "Save", keys: "⌘S"),
            ShortcutCatalogRow(action: "Attach Files…", keys: "⇧⌘A"),
            ShortcutCatalogRow(action: "Add Proton Account…", keys: ""),
            ShortcutCatalogRow(action: "Import…", keys: "⇧⌘I"),
        ]),
        ShortcutCatalogSection(title: "Edit", rows: [
            ShortcutCatalogRow(action: "Undo", keys: "⌘Z"),
            ShortcutCatalogRow(action: "Redo", keys: "⇧⌘Z"),
            ShortcutCatalogRow(action: "Cut", keys: "⌘X"),
            ShortcutCatalogRow(action: "Copy", keys: "⌘C"),
            ShortcutCatalogRow(action: "Paste", keys: "⌘V"),
            ShortcutCatalogRow(action: "Paste as Markdown…", keys: "⌥⌘V"),
            ShortcutCatalogRow(action: "Select All", keys: "⌘A"),
            ShortcutCatalogRow(action: "Insert Image…", keys: ""),
            ShortcutCatalogRow(action: "Edit in External Editor", keys: "⇧⌘E"),
            ShortcutCatalogRow(action: "Search Mail", keys: "⌥⌘F"),
            ShortcutCatalogRow(action: "Find…", keys: "⌘F"),
            ShortcutCatalogRow(action: "Find Next", keys: "⌘G"),
            ShortcutCatalogRow(action: "Find Previous", keys: "⇧⌘G"),
        ]),
        ShortcutCatalogSection(title: "View", rows: [
            ShortcutCatalogRow(action: "Show or Hide Sidebar", keys: "⌃⌘S"),
            ShortcutCatalogRow(action: "Message Source", keys: "⌥⌘U"),
            ShortcutCatalogRow(action: "Reading View", keys: "⌥⌘R"),
            ShortcutCatalogRow(action: "Enter Full Screen", keys: "⌃⌘F"),
        ]),
        ShortcutCatalogSection(title: "Mailbox", rows: [
            ShortcutCatalogRow(action: "Inbox", keys: "⌘1"),
            ShortcutCatalogRow(action: "Flagged", keys: "⌘2"),
            ShortcutCatalogRow(action: "Drafts (local)", keys: "⌘3"),
            ShortcutCatalogRow(action: "Out Box", keys: "⌘4"),
        ]),
        ShortcutCatalogSection(title: "Message", rows: [
            ShortcutCatalogRow(action: "Send", keys: "⇧⌘D"),
            ShortcutCatalogRow(action: "Open in New Window", keys: "⌘O"),
            ShortcutCatalogRow(action: "Reply", keys: "⌘R"),
            ShortcutCatalogRow(action: "Reply All", keys: "⇧⌘R"),
            ShortcutCatalogRow(action: "Forward", keys: "⇧⌘F"),
            ShortcutCatalogRow(action: "Mark as Read or Unread", keys: "⇧⌘U"),
            ShortcutCatalogRow(action: "Star or Unstar", keys: "⇧⌘L"),
            ShortcutCatalogRow(action: "Archive", keys: "⌃⌘A"),
            ShortcutCatalogRow(action: "Move to Trash", keys: "⌘⌫"),
            ShortcutCatalogRow(action: "Move to…", keys: "⇧⌘M"),
        ]),
        ShortcutCatalogSection(title: "Format", rows: [
            ShortcutCatalogRow(action: "Make Plain Text or Markdown", keys: "⇧⌘T"),
            ShortcutCatalogRow(action: "Bold", keys: "⌘B"),
            ShortcutCatalogRow(action: "Italic", keys: "⌘I"),
        ]),
        ShortcutCatalogSection(title: "Window", rows: [
            ShortcutCatalogRow(action: "Message Viewer", keys: "⌘0"),
            ShortcutCatalogRow(action: "Activity", keys: "⌥⌘0"),
        ]),
        ShortcutCatalogSection(title: "Reader and lists", rows: [
            ShortcutCatalogRow(action: "Open the selected message", keys: "Return"),
            ShortcutCatalogRow(action: "Page the reader down or up", keys: "Space / ⇧Space"),
            ShortcutCatalogRow(action: "Move through messages", keys: "↑ ↓"),
            ShortcutCatalogRow(action: "Choose a filing destination", keys: "⇧⌘M, then ↑ ↓ Return"),
        ]),
    ]
}

/// The reference's window content.
struct ShortcutHelpView: View {
    var body: some View {
        VStack(alignment: .leading, spacing: 0) {
            VStack(alignment: .leading, spacing: 6) {
                Text("Keyboard Shortcuts")
                    .font(.title2.bold())
                Text("Commands are available where they apply; a few need a message, a selection or an editable field.")
                    .font(.footnote)
                    .foregroundStyle(.secondary)
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)

            List {
                ForEach(ShortcutCatalog.sections) { section in
                    Section(section.title) {
                        ForEach(section.rows) { row in
                            HStack {
                                Text(row.action)
                                Spacer(minLength: 16)
                                if !row.keys.isEmpty {
                                    Text(row.keys)
                                        .font(.system(.body, design: .monospaced))
                                        .foregroundStyle(.secondary)
                                }
                            }
                            .accessibilityElement(children: .ignore)
                            .accessibilityLabel(
                                row.keys.isEmpty
                                    ? row.action
                                    : "\(row.action), \(row.keys)"
                            )
                        }
                    }
                }
            }
            .accessibilityIdentifier("shortcutHelpList")
        }
        .frame(minWidth: 620, minHeight: 480)
    }
}

/// One shared instance, like the Activity window: reopening brings the
/// same window forward instead of stacking copies.
@MainActor
final class ShortcutHelpWindowController: NSWindowController {
    static let title = "Keyboard Shortcuts"
    private static let autosaveName = "ShortcutHelpWindowFrame"
    private static let contentSize = NSSize(width: 640, height: 600)

    init() {
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.contentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = Self.title
        let restored = window.setFrameUsingName(Self.autosaveName)
        window.setFrameAutosaveName(Self.autosaveName)
        window.isReleasedWhenClosed = false
        super.init(window: window)
        window.contentView = NSHostingView(rootView: ShortcutHelpView())
        window.setAccessibilityIdentifier("shortcutHelp")
        if !restored { window.center() }
    }

    required init?(coder: NSCoder) {
        fatalError("ShortcutHelpWindowController is built in code")
    }
}
