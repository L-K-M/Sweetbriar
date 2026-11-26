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

private func message(
    subject: String?,
    from: [ReaderAddress] = [ReaderAddress(name: "Ada Lovelace", address: "ada@example.test")],
    to: [ReaderAddress] = [ReaderAddress(name: nil, address: "to@example.test")],
    cc: [ReaderAddress] = []
) -> ReaderMessage {
    ReaderMessage(
        messageId: 1,
        headers: ReaderHeaders(
            from: from, to: to, cc: cc, date: 1_792_238_400_000, subject: subject, messageId: nil),
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
        for symbol in ["flag", "tray.and.arrow.up", "folder.badge.gearshape", "tag", "folder"] {
            #expect(NSImage(systemSymbolName: symbol, accessibilityDescription: nil) != nil)
        }
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
        func location(of text: String) throws -> Int {
            let range = (header.string as NSString).range(of: text)
            return try #require(range.location == NSNotFound ? nil : range.location)
        }
        func font(_ text: String) throws -> NSFont {
            let index = try location(of: text)
            return try #require(header.attribute(.font, at: index, effectiveRange: nil) as? NSFont)
        }
        #expect(try font("Quarterly").fontDescriptor.symbolicTraits.contains(.bold))
        #expect(try font("Quarterly").pointSize > NSFont.systemFontSize)
        #expect(try font("Ada Lovelace").fontDescriptor.symbolicTraits.contains(.bold))
        #expect(try !font("ada@example").fontDescriptor.symbolicTraits.contains(.bold))
        let label = try location(of: "To:")
        #expect(header.attribute(.foregroundColor, at: label, effectiveRange: nil) as? NSColor
            == .secondaryLabelColor)

        let copied = ReaderViewController.headerText(
            message(subject: nil, cc: [ReaderAddress(name: "Cy", address: "cy@example.test")]))
        #expect(copied.string.hasPrefix("(No Subject)\n"))
        #expect(copied.string.contains("\nCc: Cy <cy@example.test>\n"))
    }

    @Test func aLongRecipientListCollapsesBehindShowAllRecipients() throws {
        let many = (1...18).map { ReaderAddress(name: "Person \($0)", address: "p\($0)@example.test") }
        let copies = (1...5).map { ReaderAddress(name: "Copy \($0)", address: "c\($0)@example.test") }
        let all = message(subject: "All-hands", to: many, cc: copies)
        let collapsed = ReaderViewController.headerText(all, recipientLimit: 3)
        #expect(collapsed.string.contains(
            "To: Person 1 <p1@example.test>, Person 2 <p2@example.test>, Person 3 <p3@example.test>, and 20 more\n"))
        #expect(!collapsed.string.contains("Person 4"))
        #expect(!collapsed.string.contains("Cc:"))
        #expect(ReaderViewController.hiddenRecipients(in: all, limit: 3) == 20)
        #expect(ReaderViewController.hiddenRecipients(in: all, limit: nil) == 0)
        let expanded = ReaderViewController.headerText(all)
        #expect(expanded.string.contains("Person 18 <p18@example.test>"))
        #expect(expanded.string.contains("Copy 5 <c5@example.test>"))

        // A short list hides nothing and reads exactly as before.
        let short = message(subject: "Lunch", to: Array(many.prefix(2)), cc: Array(copies.prefix(1)))
        #expect(ReaderViewController.hiddenRecipients(in: short, limit: 3) == 0)
        #expect(ReaderViewController.headerText(short, recipientLimit: 3).string == ReaderViewController.headerText(short).string)

        // A long cc under a to that fits collapses on its own.
        let copied = message(subject: "Copies", to: Array(many.prefix(3)), cc: copies)
        let line = ReaderViewController.headerText(copied, recipientLimit: 3)
        #expect(line.string.contains(
            "Cc: Copy 1 <c1@example.test>, Copy 2 <c2@example.test>, Copy 3 <c3@example.test>, and 2 more\n"))
        #expect(!line.string.contains("Copy 4"))
        #expect(ReaderViewController.hiddenRecipients(in: copied, limit: 3) == 2)

        // The button shows for any message that hides recipients, and
        // only for those, whichever state the header is in.
        #expect(ReaderViewController.hidesRecipients(all))
        #expect(ReaderViewController.hidesRecipients(copied))
        #expect(!ReaderViewController.hidesRecipients(short))
        #expect(!ReaderViewController.hidesRecipients(message(subject: "None", to: [], cc: [])))
    }

    @Test func theHeaderSkipsMissingAddressLines() {
        let header = ReaderViewController.headerText(message(subject: "Notice", from: [], to: []))
        let date = Date(timeIntervalSince1970: 1_792_238_400).formatted(date: .abbreviated, time: .shortened)
        #expect(header.string == "Notice\n\(date)")
    }

    @Test func theWindowMenuInvokesTheViewerCommand() throws {
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

    @Test func aTintedHeaderTakesTheTintsForeground() throws {
        let header = ReaderViewController.headerText(message(subject: "S"), primary: .white, secondary: .gray)
        #expect(header.attribute(.foregroundColor, at: 0, effectiveRange: nil) as? NSColor == .white)
        let range = (header.string as NSString).range(of: "From:")
        let label = try #require(range.location == NSNotFound ? nil : range.location)
        #expect(header.attribute(.foregroundColor, at: label, effectiveRange: nil) as? NSColor == .gray)
    }
}
