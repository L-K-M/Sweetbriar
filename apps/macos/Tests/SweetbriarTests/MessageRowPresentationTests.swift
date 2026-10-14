// B2a's shared presentation under test (plan M0.10): the
// context-click target rule, the VoiceOver label order, the monogram
// and account color, the visible-rank math and the row content's
// blank-field stand-ins. Foundation only.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
import Foundation
import Testing

@Suite("the context-click target rule")
struct ContextClickTargetTests {
    @Test("a click inside the selection targets the whole selection")
    func inside() {
        let selection: Set<Int64> = [10, 20, 30]
        #expect(ContextClickTarget.targets(clickedID: 20, selection: selection) == selection)
    }

    @Test("a click outside the selection targets that row alone")
    func outside() {
        let selection: Set<Int64> = [10, 20, 30]
        #expect(ContextClickTarget.targets(clickedID: 99, selection: selection) == [99])
        #expect(ContextClickTarget.targets(clickedID: 99, selection: []) == [99])
    }

    @Test("the disabled header counts the targets")
    func summary() {
        #expect(ContextClickTarget.targetSummary(count: 1) == "1 message")
        let many = ContextClickTarget.targetSummary(count: 2)
        #expect(many.hasPrefix("2"))
        #expect(many.hasSuffix(" messages"))
        #expect(!ContextClickTarget.actionTitles.isEmpty)
    }
}

@Suite("the shared row presentation")
@MainActor
struct MessageRowPresentationTests {
    @Test("the VoiceOver label order: sender, subject, date, flags")
    func labelOrder() {
        #expect(
            MessageRowFormatter.accessibilityLabel(
                sender: "Ada", subject: "Hi", date: "10:00",
                unread: true, flagged: true
            ) == "Ada, Hi, 10:00, unread, flagged"
        )
        #expect(
            MessageRowFormatter.accessibilityLabel(
                sender: "Ada", subject: "Hi", date: "10:00",
                unread: false, flagged: false
            ) == "Ada, Hi, 10:00"
        )
    }

    @Test("the monogram is the account id's last component's letter")
    func monogram() {
        #expect(MessageRowFormatter.monogram(for: "synthetic-b") == "B")
        #expect(MessageRowFormatter.monogram(for: "synthetic") == "S")
        #expect(MessageRowFormatter.monogram(for: "") == "?")
    }

    @Test("the account color index is deterministic and in range")
    func accountColor() {
        let index = MessageRowFormatter.accountColorIndex(for: "synthetic-a")
        #expect((0 ..< MessageRowMetrics.accountColorCount).contains(index))
        #expect(index == MessageRowFormatter.accountColorIndex(for: "synthetic-a"))
        // FNV-1a, not hashValue — a known input, a known slot.
        #expect(index == 4)
    }

    @Test("visibleRanks: partial rows included, clamped to the list")
    func visibleRanks() {
        // 640 pt at 56 pt pitch → twelve rows from the top.
        #expect(MessageRowMetrics.visibleRanks(top: 0, height: 640, count: 500_000) == 0 ..< 12)
        #expect(
            MessageRowMetrics.visibleRanks(top: 560, height: 640, count: 500_000)
                == 10 ..< 22
        )
        // Scrolled to the end: clamped to the row count.
        #expect(
            MessageRowMetrics.visibleRanks(
                top: 56 * 499_995, height: 640, count: 500_000
            ) == 499_995 ..< 500_000
        )
        #expect(MessageRowMetrics.visibleRanks(top: 0, height: 0, count: 10).isEmpty)
        #expect(MessageRowMetrics.visibleRanks(top: -100, height: 640, count: 10) == 0 ..< 10)
    }

    @Test("blank fields get stand-ins; the label carries the flags")
    func present() {
        let formatter = MessageRowFormatter()
        let now = Date(timeIntervalSince1970: 1_760_000_000)
        let content = formatter.present(
            sender: "  ", subject: "Subject", snippet: "",
            dateMilliseconds: 1_760_000_000_000,
            unread: true, flagged: false, hasAttachments: true,
            tags: ["tag"], accountID: "synthetic-a", now: now
        )
        #expect(content.sender == "(no sender)")
        #expect(content.subject == "Subject")
        #expect(content.snippet == "")
        #expect(content.unread && content.hasAttachments)
        #expect(content.accessibilityLabel.hasPrefix("(no sender), Subject, "))
        #expect(content.accessibilityLabel.hasSuffix(", unread"))
        #expect(content.tags == ["tag"])
        #expect(content.monogram == "A")
    }
}
