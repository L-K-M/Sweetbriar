// Every command carries a stable identifier in the
// `ch.lkmc.Sweetbriar.cmd.*` namespace (spec/08 "Menus and commands").

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import Testing

@MainActor
private final class Running {
    var value = true
}

@Suite("command identifiers")
@MainActor
struct CommandIdentifierTests {
    /// AppKit's own commands, which keep AppKit's names.
    private static let appKitActions: Set<String> = [
        "orderFrontStandardAboutPanel:", "hide:", "hideOtherApplications:",
        "unhideAllApplications:", "terminate:", "undo:", "redo:", "cut:", "copy:", "paste:",
        "selectAll:", "performMiniaturize:", "performClose:",
    ]

    private static func items(_ menu: NSMenu) -> [NSMenuItem] {
        menu.items.flatMap { item in [item] + (item.submenu.map(items) ?? []) }
    }

    /// Each of Sweetbriar's own menu bar commands has its own
    /// identifier in the namespace; none is shared.
    @Test func everyMenuBarCommandHasItsOwnIdentifier() throws {
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {}
        )
        let bar = builder.makeMainMenu()
        // AppKit fills the registered Window menu with its own items (the
        // window list, tiling); Sweetbriar's one command there is checked
        // by name.
        let window = try #require(bar.items.compactMap(\.submenu).first { $0.title == "Window" })
        let activity = try #require(window.items.first { $0.title == "Activity" })
        #expect(activity.identifier == MainMenuBuilder.ItemID.showActivity)
        // An item holding a submenu reports AppKit's `submenuAction:`;
        // it opens the submenu and isn't a command.
        let commands = Self.items(bar).filter { item in
            guard let action = item.action, item.submenu == nil, item.menu !== window else {
                return false
            }
            return !Self.appKitActions.contains(NSStringFromSelector(action))
        } + [activity]
        #expect(commands.count >= 25, "the menu bar's commands were found")
        for item in commands {
            let identifier = item.identifier?.rawValue ?? ""
            #expect(
                identifier.hasPrefix("ch.lkmc.Sweetbriar.cmd."),
                "\(item.title) has \(identifier.isEmpty ? "no identifier" : identifier)")
        }
        let identifiers = commands.compactMap(\.identifier?.rawValue)
        #expect(Set(identifiers).count == identifiers.count, "no two commands share one")
    }

    /// The benchmarks still share their in-flight gate, now through
    /// their own identifiers.
    @Test func theBenchmarksStayGatedByTheirOwnIdentifiers() throws {
        let running = Running()
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { !running.value }, onShowComposerProbe: {}, onImport: {}
        )
        let debug = try #require(
            builder.makeMainMenu().items.compactMap(\.submenu).first { $0.title == "Debug" })
        let benchmarks = debug.items.filter { $0.title.hasPrefix("Run ") }
        #expect(
            Set(benchmarks.compactMap(\.identifier)) == MainMenuBuilder.ItemID.benchmarks)
        #expect(benchmarks.allSatisfy { !builder.validateMenuItem($0) })
        running.value = false
        #expect(benchmarks.allSatisfy { builder.validateMenuItem($0) })
    }
}
#endif
