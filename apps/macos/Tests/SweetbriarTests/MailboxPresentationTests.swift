import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private func entry(_ target: SidebarModel.Target) -> SidebarModel.Entry {
    SidebarModel.Entry(id: "a/1", title: "Entry", unread: nil, target: target)
}

private func mailbox(role: MailboxRole?, kind: MailboxKind) -> Mailbox {
    Mailbox(
        mailboxId: 1, accountId: "a", name: "Box", delimiter: "/", role: role, kind: kind,
        selectable: true, total: 0, unread: 0)
}

private func message(subject: String?, cc: [ReaderAddress] = []) -> ReaderMessage {
    ReaderMessage(
        messageId: 1,
        headers: ReaderHeaders(
            from: [ReaderAddress(name: "Ada Lovelace", address: "ada@example.test")],
            to: [ReaderAddress(name: nil, address: "to@example.test")],
            cc: cc, date: 1_792_238_400_000, subject: subject, messageId: nil),
        accountId: "a", accountDisplayName: "Work", occurrences: [], threadId: nil, tags: [],
        note: nil, bodyAvailability: .acquired)
}

@MainActor
private final class Shown {
    var value = false
}

/// The sidebar's symbols and the reader's header (spec/08 "Sidebar",
/// Reader "Header area").
@Suite @MainActor
struct MailboxPresentationTests {
    @Test func eachMailboxShowsItsRolesSymbol() {
        #expect(SidebarSymbol.name(for: entry(.window(.unified(.inbox))), mailbox: nil) == "tray")
        #expect(SidebarSymbol.name(for: entry(.window(.flagged)), mailbox: nil) == "flag")
        #expect(SidebarSymbol.name(for: entry(.drafts), mailbox: nil) == "doc")
        #expect(SidebarSymbol.name(for: entry(.outBox), mailbox: nil) == "tray.and.arrow.up")
        #expect(SidebarSymbol.name(for: entry(.window(.view("v"))), mailbox: nil) == "folder.badge.gearshape")

        let box = entry(.window(.mailbox(1)))
        let roles: [(MailboxRole, String)] = [
            (.inbox, "tray"), (.sent, "paperplane"), (.drafts, "doc"), (.trash, "trash"),
            (.junk, "xmark.bin"), (.archive, "archivebox"), (.all, "tray.full"), (.flagged, "star"),
        ]
        for (role, symbol) in roles {
            #expect(SidebarSymbol.name(for: box, mailbox: mailbox(role: role, kind: .folder)) == symbol)
            #expect(NSImage(systemSymbolName: symbol, accessibilityDescription: nil) != nil)
        }
        // Without a role, the kind decides.
        #expect(SidebarSymbol.name(for: box, mailbox: mailbox(role: nil, kind: .label)) == "tag")
        #expect(SidebarSymbol.name(for: box, mailbox: mailbox(role: nil, kind: .folder)) == "folder")
        #expect(SidebarSymbol.name(for: box, mailbox: nil) == "folder")
        #expect(SidebarSymbol.name(for: entry(.unavailable(note: "n")), mailbox: nil) == "folder")
    }

    @Test func theHeaderLeadsWithTheSubjectAndTheSendersName() throws {
        let date = Date(timeIntervalSince1970: 1_792_238_400).formatted(date: .abbreviated, time: .shortened)
        let header = ReaderViewController.headerText(message(subject: "  Quarterly notes "))
        #expect(header.string == """
            Quarterly notes
            From: Ada Lovelace <ada@example.test>
            To: to@example.test
            \(date)
            """)
        let font = { (text: String) throws -> NSFont in
            let range = (header.string as NSString).range(of: text)
            return try #require(header.attribute(.font, at: range.location, effectiveRange: nil) as? NSFont)
        }
        #expect(try font("Quarterly").fontDescriptor.symbolicTraits.contains(.bold))
        #expect(try font("Quarterly").pointSize > NSFont.systemFontSize)
        #expect(try font("Ada Lovelace").fontDescriptor.symbolicTraits.contains(.bold))
        #expect(try !font("ada@example").fontDescriptor.symbolicTraits.contains(.bold))
        let label = (header.string as NSString).range(of: "To:")
        #expect(header.attribute(.foregroundColor, at: label.location, effectiveRange: nil) as? NSColor
            == .secondaryLabelColor)

        let copied = ReaderViewController.headerText(
            message(subject: nil, cc: [ReaderAddress(name: "Cy", address: "cy@example.test")]))
        #expect(copied.string.hasPrefix("(No Subject)\n"))
        #expect(copied.string.contains("\nCc: Cy <cy@example.test>\n"))
    }

    /// Window → Message Viewer brings the main window back once it was
    /// closed, as Mail's does.
    @Test func theWindowMenuBringsTheViewerBack() throws {
        let shown = Shown()
        // Kept: menu items hold their target weakly.
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {},
            onShowViewer: { shown.value = true }
        )
        let bar = builder.makeMainMenu()
        let window = try #require(bar.items.compactMap(\.submenu).first { $0.title == "Window" })
        let viewer = try #require(window.items.first { $0.title == "Message Viewer" })
        #expect(viewer.keyEquivalent == "0" && viewer.keyEquivalentModifierMask == [.command])
        #expect(viewer.identifier == MainMenuBuilder.ItemID.showViewer)
        let action = try #require(viewer.action)
        #expect(NSApp.sendAction(action, to: viewer.target, from: viewer))
        #expect(shown.value)
        withExtendedLifetime(builder) {}
    }

    @Test func aTintedHeaderTakesTheTintsForeground() {
        let header = ReaderViewController.headerText(message(subject: "S"), primary: .white, secondary: .gray)
        #expect(header.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor == .white)
        let label = (header.string as NSString).range(of: "From:")
        #expect(header.attribute(.foregroundColor, at: label.location, effectiveRange: nil) as? NSColor == .gray)
    }
}
