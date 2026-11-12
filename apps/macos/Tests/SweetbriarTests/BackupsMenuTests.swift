import AppKit
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@MainActor
private final class BackupsMenuProbe { var writable = false }

@Suite
@MainActor
struct BackupsMenuTests {
    @Test func writeMenusFollowHealthWhileReadAndRecoveryNavigationStayAvailable() throws {
        let probe = BackupsMenuProbe()
        let oldWindowMenu = NSApp.windowsMenu
        defer { NSApp.windowsMenu = oldWindowMenu }
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {},
            isWriteEnabled: { probe.writable }
        )
        func descendants(_ menu: NSMenu) -> [NSMenuItem] {
            menu.items.flatMap { [$0] + ($0.submenu.map(descendants) ?? []) }
        }
        let items = descendants(builder.makeMainMenu())
        let names = ["Import…", "Add Proton Bridge Account…", "Fill 500,000 Synthetic Rows"]
        let writes = items.filter { names.contains($0.title) }
        #expect(writes.count == names.count)
        for item in writes { #expect(!builder.validateMenuItem(item)) }
        for name in ["Settings…", "List Prototype"] {
            let item = try #require(items.first { $0.title == name })
            #expect(builder.validateMenuItem(item))
        }
        probe.writable = true
        for item in writes { #expect(builder.validateMenuItem(item)) }
        let benchmark = try #require(items.first { $0.title == "Run List Benchmark" })
        #expect(!builder.validateMenuItem(benchmark), "diagnostic admission remains a separate contract")
    }
}
