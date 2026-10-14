// The presentation every prototype row shares (plan M0.10): one row
// pitch, one date rule, the VoiceOver label order, the account color
// index, the context-click target rule and the placeholder geometry —
// identical in the AppKit and SwiftUI lists. Foundation only, so it
// type-checks on Linux and part B2b's test bundle can drive it.

import Foundation

#if canImport(SweetbriarBridge)
import SweetbriarBridge
#endif

/// Layout constants both prototype lists share (plan M0.10: the same
/// fields, height and colors in each).
enum MessageRowMetrics {
    /// Row pitch in points — the distance between consecutive rows'
    /// tops, equal in both lists.
    static let pitch: CGFloat = 56

    /// The account rail's width along the row's leading edge
    /// (spec/08 "Rows": a 4 pt colored rail plus monogram).
    static let railWidth: CGFloat = 4

    /// The monogram disc's diameter.
    static let monogramSize: CGFloat = 26

    /// Padding between the rail and the monogram, and between the
    /// monogram and the text column.
    static let innerGap: CGFloat = 8

    /// Padding between a row's content and its trailing edge.
    static let trailingPadding: CGFloat = 8

    /// The unread dot's diameter.
    static let unreadDotSize: CGFloat = 8

    /// The flag and paperclip marker size.
    static let markerSize: CGFloat = 13

    /// The gap between two trailing markers.
    static let markerSpacing: CGFloat = 4

    /// How many palette slots `accountColorIndex` rotates through. The
    /// AppKit and SwiftUI palettes are declared in matching order.
    static let accountColorCount = 6

    /// The placeholder block's horizontal inset (spec/08: same height,
    /// no text, a subtle fill).
    static let placeholderInsetX: CGFloat = 8

    /// The placeholder block's vertical inset.
    static let placeholderInsetY: CGFloat = 6

    /// The placeholder block's corner radius.
    static let placeholderCornerRadius: CGFloat = 6

    /// The placeholder fill's opacity over the label color.
    static let placeholderFillOpacity: CGFloat = 0.06

    /// The tag chip's inner horizontal padding.
    static let tagChipPadding: CGFloat = 5

    /// The ranks a `height`-tall viewport shows at scroll offset `top`
    /// in a `count`-row list at `pitch` spacing — partial rows
    /// included, clamped to the row range. Both lists feed
    /// `ListWindowModel.setVisible` and the `visibleRanks` seam from
    /// this, so the model and the harness see the same range.
    static func visibleRanks(top: CGFloat, height: CGFloat, count: Int) -> Range<Int> {
        guard height > 0, count > 0 else { return 0 ..< 0 }
        let first = min(count, max(0, Int(floor(top / pitch))))
        let last = min(count, max(first, Int(ceil((top + height) / pitch))))
        return first ..< last
    }
}

/// Everything a row draws, precomputed once per row so both lists show
/// identical strings in identical order (plan M0.10).
struct MessageRowContent: Equatable {
    /// Display strings — a trimmed-empty field gets a stand-in so the
    /// row never reads blank.
    var sender: String
    var subject: String
    var snippet: String

    /// The row's date per the shared rule.
    var date: String

    /// The VoiceOver label (plan M0.10): sender, subject, date, then
    /// "unread" and "flagged" when set, comma-separated.
    var accessibilityLabel: String

    var unread: Bool
    var flagged: Bool
    var hasAttachments: Bool
    var tags: [String]

    /// Which slot of the platform's account palette `accountID` takes.
    var accountColorIndex: Int

    /// The rail's one-letter monogram.
    var monogram: String
}

/// Formats `ListRow` fields into `MessageRowContent` (plan M0.10):
/// the date rule — today shows the time, this year the day and month,
/// older a short date — the VoiceOver label, the account color index
/// and monogram, and the context menu's target count. Main-actor
/// bound: it holds the one cached `DateFormatter` and the count
/// formatter so a fast-scrolling list never re-creates them.
@MainActor
final class MessageRowFormatter {
    /// The one cached formatter; its format string is swapped per
    /// bucket — `DateFormatter` must not be re-created per row.
    private let dateFormatter = DateFormatter()

    /// The bucket format strings, resolved once through the locale's
    /// templates so they order fields the way the locale expects.
    private let todayFormat: String
    private let thisYearFormat: String
    private let olderFormat: String

    init() {
        todayFormat = DateFormatter.dateFormat(
            fromTemplate: "jmm", options: 0, locale: .current
        ) ?? "HH:mm"
        thisYearFormat = DateFormatter.dateFormat(
            fromTemplate: "dMMM", options: 0, locale: .current
        ) ?? "d MMM"
        olderFormat = DateFormatter.dateFormat(
            fromTemplate: "yMd", options: 0, locale: .current
        ) ?? "yyyy-MM-dd"
    }

    /// One row's presentable content. `sortDate` is epoch milliseconds
    /// (spec/07); `now` is injectable for tests.
    func present(
        sender: String,
        subject: String,
        snippet: String,
        dateMilliseconds: Int64,
        unread: Bool,
        flagged: Bool,
        hasAttachments: Bool,
        tags: [String],
        accountID: String,
        now: Date = Date()
    ) -> MessageRowContent {
        let shownSender = Self.display(sender, fallback: "(no sender)")
        let shownSubject = Self.display(subject, fallback: "(no subject)")
        let shownSnippet = Self.display(snippet, fallback: "")
        let date = dateString(dateMilliseconds, now: now)
        return MessageRowContent(
            sender: shownSender,
            subject: shownSubject,
            snippet: shownSnippet,
            date: date,
            accessibilityLabel: Self.accessibilityLabel(
                sender: shownSender,
                subject: shownSubject,
                date: date,
                unread: unread,
                flagged: flagged
            ),
            unread: unread,
            flagged: flagged,
            hasAttachments: hasAttachments,
            tags: tags,
            accountColorIndex: Self.accountColorIndex(for: accountID),
            monogram: Self.monogram(for: accountID)
        )
    }


    /// The VoiceOver label (plan M0.10): sender, subject, date, then
    /// "unread" and "flagged" when set, comma-separated.
    static func accessibilityLabel(
        sender: String,
        subject: String,
        date: String,
        unread: Bool,
        flagged: Bool
    ) -> String {
        var parts = [sender, subject, date]
        if unread { parts.append("unread") }
        if flagged { parts.append("flagged") }
        return parts.joined(separator: ", ")
    }

    /// The account's palette slot — a stable FNV-1a hash, not
    /// `hashValue` (which is seeded per process), so an account is the
    /// same color in every launch and in both lists.
    static func accountColorIndex(for accountID: String) -> Int {
        var hash: UInt64 = 0xcbf2_9ce4_8422_2325
        for byte in accountID.utf8 {
            hash = (hash ^ UInt64(byte)) &* 0x0000_0100_0000_01b3
        }
        return Int(hash % UInt64(MessageRowMetrics.accountColorCount))
    }

    /// The rail's one-letter monogram: the first letter of the
    /// account id's last `-` component, uppercased — "synthetic-b"
    /// reads "B", an undivided id its own first letter.
    static func monogram(for accountID: String) -> String {
        let component = accountID.split(separator: "-").last
            ?? Substring(accountID)
        guard let first = component.first else { return "?" }
        return String(first).uppercased()
    }

    /// The shared date rule: today → time, this year → day and month,
    /// else a short date (plan M0.10).
    private func dateString(_ milliseconds: Int64, now: Date) -> String {
        let date = Date(timeIntervalSince1970: TimeInterval(milliseconds) / 1_000)
        let calendar = Calendar.current
        if calendar.isDate(date, inSameDayAs: now) {
            dateFormatter.dateFormat = todayFormat
        } else if calendar.isDate(date, equalTo: now, toGranularity: .year) {
            dateFormatter.dateFormat = thisYearFormat
        } else {
            dateFormatter.dateFormat = olderFormat
        }
        return dateFormatter.string(from: date)
    }

    /// A trimmed-empty field draws as `fallback`, so a row never
    /// renders a blank line.
    private static func display(_ value: String, fallback: String) -> String {
        value.trimmingCharacters(in: .whitespaces).isEmpty ? fallback : value
    }
}

/// spec/08 "Selection": a context click on a selected row targets the
/// whole selection; on an unselected row it targets that row alone and
/// the selection stays untouched. Both lists run this rule and share
/// the menu's strings; part B2b's tests pin it.
enum ContextClickTarget {
    /// The menu's action items (spec/08): no-ops — only the disabled
    /// header carries information (the target count). One list, one
    /// order, so the two menus can't drift.
    static let actionTitles = [
        "Mark as Read", "Flag", "Archive", "Move to Trash",
    ]

    /// The occurrence ids the context menu's commands act on.
    static func targets(clickedID: Int64, selection: Set<Int64>) -> Set<Int64> {
        selection.contains(clickedID) ? selection : [clickedID]
    }

    /// The menu's disabled header (plan M0.10): the count the items
    /// act on, so the owner can check the context-click rule —
    /// "1 message", "9,990 messages".
    static func targetSummary(count: Int) -> String {
        guard count != 1 else { return "1 message" }
        return "\(count.formatted(.number)) messages"
    }
}

/// A `RandomAccessCollection` over a window's id index for the SwiftUI
/// `ForEach` (plan M0.10 guardrails): elements — (rank, occurrence id)
/// — are made on access and identified by occurrence id, so there is
/// no 500k-element array of wrappers and no positional identity. The
/// wrapped array is copy-on-write: constructing it is O(1).
struct RankedIDs: RandomAccessCollection {
    /// One list element: the window rank plus the row's stable id.
    struct Element: Identifiable, Equatable {
        var rank: Int
        var id: Int64
    }

    private let ids: [Int64]

    init(_ ids: [Int64]) {
        self.ids = ids
    }

    var startIndex: Int { ids.startIndex }
    var endIndex: Int { ids.endIndex }

    func index(after index: Int) -> Int { index + 1 }
    func index(before index: Int) -> Int { index - 1 }

    subscript(position: Int) -> Element {
        Element(rank: position, id: ids[position])
    }
}

#if canImport(SweetbriarBridge)
extension MessageRowFormatter {
    /// `present` straight off a bridge `ListRow` — the convenience the
    /// lists call; kept behind `canImport` so this file type-checks
    /// without the framework on Linux.
    func present(_ row: ListRow, now: Date = Date()) -> MessageRowContent {
        present(
            sender: row.sender,
            subject: row.subject,
            snippet: row.snippet,
            dateMilliseconds: row.sortDate,
            unread: row.unread,
            flagged: row.flagged,
            hasAttachments: row.hasAttachments,
            tags: row.tags,
            accountID: row.accountId,
            now: now
        )
    }
}
#endif
