import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private struct ChromeClock: FrameClock { func nextFrame() async { await Task.yield() } }

/// A target that admits only the actions it was told to.
@MainActor
private final class ChromeTarget: NSObject, NSToolbarItemValidation, MessageFilingCommands {
    var enabled: Set<Selector> = []

    func validateToolbarItem(_ item: NSToolbarItem) -> Bool {
        item.action.map(enabled.contains) ?? false
    }

    func archiveMessages(_ sender: Any?) {}
    func trashMessages(_ sender: Any?) {}
    func moveMessages(_ sender: Any?) {}
    func addLabel(_ sender: Any?) {}
    func removeLabel(_ sender: Any?) {}
}

/// The main window's chrome (spec/08 "Main window"): its title and
/// subtitle, its toolbar and the integrity banner under the toolbar.
@Suite(.serialized, .timeLimit(.minutes(1))) @MainActor
struct MainWindowChromeTests {
    private static let inbox = SidebarModel.Entry(
        id: "a/inbox", title: "Inbox", unread: 3, target: .window(.mailbox(1)))

    @Test func theTitleNamesThePaneAndTheSubtitleCountsIt() {
        let title = MainWindowController.titlePresentation
        #expect(title(.drafts, false, nil, nil, nil) == ("Drafts (local)", ""))
        #expect(title(.outBox, true, Self.inbox, "Work", 9) == ("Out Box", ""))
        #expect(title(.messages, true, Self.inbox, "Work", nil) == ("Search", ""))
        #expect(title(.messages, true, Self.inbox, "Work", 1) == ("Search", "1 result"))
        // Counts use the user's number format.
        #expect(title(.messages, true, nil, nil, 1_234) == ("Search", "\(1_234.formatted()) results"))
        #expect(title(.messages, false, Self.inbox, "Work", 240) == ("Inbox", "Work · 240 messages, 3 unread"))
        #expect(title(.messages, false, Self.inbox, nil, 1) == ("Inbox", "1 message, 3 unread"))
        #expect(title(.messages, false, Self.inbox, "Work", nil) == ("Inbox", "Work"))
        var read = Self.inbox
        read.unread = 0
        #expect(title(.messages, false, read, nil, 2) == ("Inbox", "2 messages"))
        #expect(title(nil, false, nil, nil, nil) == ("Sweetbriar", ""))
    }

    @Test func theDockBadgeShowsOnlyANonZeroInboxUnreadCount() {
        #expect(MainWindowController.dockBadgeLabel(for: 0) == "")
        #expect(MainWindowController.dockBadgeLabel(for: 1) == "1")
        #expect(MainWindowController.dockBadgeLabel(for: 1_234) == "1234")
    }

    @Test func theToolbarCarriesTheMessageCommandsInGroups() throws {
        #expect(MainToolbar.defaultItems == [
            .toggleSidebar, .sidebarTrackingSeparator, .flexibleSpace, MainToolbar.newMessage,
            MainToolbar.replyGroup, MainToolbar.filingGroup, MainToolbar.labels,
            MainToolbar.flagGroup, MainToolbar.search,
        ])
        #expect(Set(MainToolbar.allowedItems).isSuperset(of: MainToolbar.defaultItems))
        #expect(MainToolbar.allowedItems.contains(.space))

        let composer = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {})
        let newMessage = try #require(MainToolbar.item(MainToolbar.newMessage, composeTarget: composer))
        #expect(newMessage.action == #selector(MainMenuBuilder.newMessage(_:)))
        #expect(newMessage.target === composer)

        let reply = try #require(
            MainToolbar.item(MainToolbar.replyGroup, composeTarget: composer) as? NSToolbarItemGroup)
        #expect(reply.subitems.map(\.action) == [
            #selector(MainMenuBuilder.reply(_:)), #selector(MainMenuBuilder.replyAll(_:)),
            #selector(MainMenuBuilder.forward(_:)),
        ])
        #expect(reply.subitems.allSatisfy { $0.target === composer })

        let filing = try #require(
            MainToolbar.item(MainToolbar.filingGroup, composeTarget: composer) as? NSToolbarItemGroup)
        #expect(filing.subitems.map(\.label) == ["Archive", "Move to Trash", "Move to…"])
        #expect(filing.subitems.allSatisfy { $0.target == nil && $0.image != nil })

        let labels = try #require(
            MainToolbar.item(MainToolbar.labels, composeTarget: composer) as? NSMenuToolbarItem)
        #expect(labels.menu.items.map(\.title) == ["Add Label…", "Remove Label…"])

        // The window makes the search item; AppKit makes its own.
        #expect(MainToolbar.item(MainToolbar.search, composeTarget: composer) == nil)
        #expect(MainToolbar.item(.toggleSidebar, composeTarget: composer) == nil)
    }

    /// AppKit leaves a group's subitems as they are: each must follow
    /// its own command, and the group is enabled while one is.
    @Test func aGroupValidatesEachOfItsCommands() throws {
        let target = ChromeTarget()
        let filing = try #require(
            MainToolbar.item(MainToolbar.filingGroup, composeTarget: nil) as? NSToolbarItemGroup)
        for item in filing.subitems { item.target = target }

        filing.validate()
        #expect(filing.subitems.allSatisfy { !$0.isEnabled })
        #expect(!filing.isEnabled)

        target.enabled = [#selector(MessageFilingCommands.trashMessages(_:))]
        filing.validate()
        #expect(filing.subitems.map(\.isEnabled) == [false, true, false])
        #expect(filing.isEnabled)

        let archive = #selector(MessageFilingCommands.archiveMessages(_:))
        let item = NSToolbarItem(itemIdentifier: MainToolbar.filingGroup)
        item.action = archive
        #expect(!MainToolbar.isEnabled(archive, target: target, item: item))
        target.enabled.insert(archive)
        #expect(MainToolbar.isEnabled(archive, target: target, item: item))
    }

    @Test func theFlagButtonsShowWhatAClickDoes() {
        #expect(MainToolbar.flagSymbol(for: "Mark as Read") == "envelope.open")
        #expect(MainToolbar.flagSymbol(for: "Mark as Unread") == "envelope.badge")
        #expect(MainToolbar.flagSymbol(for: "Star") == "star")
        #expect(MainToolbar.flagSymbol(for: "Unstar") == "star.slash")
        #expect(MainToolbar.flagSymbol(for: "Archive") == nil)
    }

    /// Regression: the banner's hosting view held the window to the
    /// empty banner's width, and the window shrank to a sliver.
    @Test func anEmptyBannerLeavesTheWindowsWidthAlone() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("sb-chrome-\(UUID().uuidString)")
        let bridge = try await CoreBridge.open(
            config: CoreConfig(
                dataRoot: root.appendingPathComponent("data").path,
                cacheRoot: root.appendingPathComponent("cache").path),
            snapshots: FrameBatcher<[Setting]>(clock: ChromeClock()) { _ in })
        let factory = SharedBackups(didRestore: {}, didClose: {})
        let window = NSWindow(
            contentRect: NSRect(x: 200, y: 200, width: 900, height: 500),
            styleMask: [.titled, .closable, .resizable, .fullSizeContentView],
            backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.toolbar = NSToolbar(identifier: "sb-chrome-test")
        window.toolbarStyle = .unified
        let result: Result<Void, any Error>
        do {
            let model = try #require(factory.model(source: bridge))
            await model.refresh()
            let accessory = IntegrityBannerAccessory(model: model, openBackups: {})
            window.addTitlebarAccessoryViewController(accessory)
            window.orderFront(nil)
            for _ in 0 ..< 5 {
                window.contentView?.layoutSubtreeIfNeeded()
                window.layoutIfNeeded()
                window.displayIfNeeded()
                try await Task.sleep(for: .milliseconds(20))
            }
            #expect(window.frame.width == 900)
            #expect(accessory.isHidden)
            result = .success(())
        } catch { result = .failure(error) }
        window.close()
        await factory.cancelAndWait()
        await bridge.shutdown()
        try? FileManager.default.removeItem(at: root)
        try result.get()
    }
}
