// The filing palette and toast on real AppKit controls (spec/08
// "Keyboard filing").

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import SweetbriarBridge
import Testing

private func mailbox(_ id: Int64, _ name: String, kind: MailboxKind = .folder) -> Mailbox {
    Mailbox(
        mailboxId: id, accountId: "a", name: name, delimiter: nil, role: nil, kind: kind,
        selectable: true, total: 0, unread: 0)
}

@MainActor
private func palette() -> FilingPalette {
    FilingPalette(
        choice: .move, mailboxes: [mailbox(1, "Inbox"), mailbox(2, "Work"), mailbox(3, "Workshop")],
        recent: [3], accountName: "Personal", account: nil)
}

@MainActor
@Suite("filing palette", .serialized)
struct FilingPaletteTests {
    @Test func listsRecentFirstWithTheFirstChosen() {
        let palette = palette()
        #expect(palette.shown.map(\.name) == ["Workshop", "Inbox", "Work"])
        #expect(palette.table.selectedRow == 0)
    }

    @Test func typingFiltersAndTheArrowsStayInRange() {
        let palette = palette()
        palette.field.stringValue = "work"
        palette.filter()
        #expect(palette.shown.map(\.name) == ["Workshop", "Work"])

        let editor = NSTextView()
        let down = #selector(NSResponder.moveDown(_:))
        #expect(palette.control(palette.field, textView: editor, doCommandBy: down))
        #expect(palette.control(palette.field, textView: editor, doCommandBy: down))
        #expect(palette.table.selectedRow == 1)

        let up = #selector(NSResponder.moveUp(_:))
        #expect(palette.control(palette.field, textView: editor, doCommandBy: up))
        #expect(palette.control(palette.field, textView: editor, doCommandBy: up))
        #expect(palette.table.selectedRow == 0)
    }

    /// Return and Escape end the sheet with the choice and with none.
    @Test func returnChoosesAndEscapeDoesNot() async throws {
        for (command, expected) in [
            (#selector(NSResponder.insertNewline(_:)), "Inbox" as String?),
            (#selector(NSResponder.cancelOperation(_:)), nil),
        ] {
            let parent = NSWindow(
                contentRect: NSRect(x: 0, y: 0, width: 500, height: 420),
                styleMask: [.titled], backing: .buffered, defer: false)
            parent.isReleasedWhenClosed = false
            defer { parent.close() }
            let palette = palette()
            var ended = false
            var chosen: Mailbox?
            palette.begin(on: parent) { mailbox in
                ended = true
                chosen = mailbox
            }
            palette.field.stringValue = "inb"
            palette.filter()

            #expect(palette.control(palette.field, textView: NSTextView(), doCommandBy: command))
            let deadline = ContinuousClock.now + .seconds(5)
            while !ended, ContinuousClock.now < deadline {
                try await Task.sleep(for: .milliseconds(10))
            }
            try #require(ended)
            #expect(chosen?.name == expected)
        }
    }

    @Test func labelChoicesListOnlyLabels() {
        let all = [mailbox(1, "Inbox"), mailbox(2, "Travel", kind: .label)]
        #expect(FilingChoice.addLabel.candidates(in: all, shownMailboxId: nil).map(\.name) == ["Travel"])
        #expect(FilingChoice.removeLabel.candidates(in: all, shownMailboxId: nil).map(\.name) == ["Travel"])
        #expect(FilingChoice.move.candidates(in: all, shownMailboxId: 1).isEmpty)
    }
}
#endif
