// Help → Keyboard Shortcuts…: the catalog carries the workflows users
// look up, and the Help menu carries the command.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import Testing

@Suite("the keyboard shortcut reference")
@MainActor
struct ShortcutHelpTests {
    @Test func coversTheCoreWorkflows() {
        let rows = ShortcutCatalog.sections.flatMap(\.rows)
        let actions = rows.map(\.action)
        for expected in [
            "New Message", "Reply", "Reply All", "Forward",
            "Archive", "Move to Trash", "Move to…",
            "Mark as Read or Unread", "Star or Unstar",
            "Search Mail", "Message Source", "Reading View",
        ] {
            #expect(actions.contains(expected), "missing \(expected)")
        }
        #expect(rows.allSatisfy { !$0.action.isEmpty })
        for section in ShortcutCatalog.sections {
            let titles = section.rows.map(\.action)
            #expect(Set(titles).count == titles.count, "\(section.title) repeats an action")
        }
    }

    @Test func everySectionHasRowsAndATitle() {
        #expect(ShortcutCatalog.sections.count >= 8)
        for section in ShortcutCatalog.sections {
            #expect(!section.title.isEmpty)
            #expect(!section.rows.isEmpty)
        }
    }

    @Test func theHelpMenuOffersIt() throws {
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {}
        )
        let help = try #require(
            builder.makeMainMenu().items.compactMap(\.submenu).first { $0.title == "Help" })
        let item = try #require(help.items.first { $0.title == "Keyboard Shortcuts…" })
        #expect(item.identifier == MainMenuBuilder.ItemID.showShortcuts)
        #expect(item.action == #selector(MainMenuBuilder.showShortcuts(_:)))
    }
}
#endif
