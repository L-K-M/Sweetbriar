// Where the filing palette can file, and in what order (spec/08
// "Keyboard filing"; spec/04-sync-proton's matrix for the
// destinations). The core checks every destination again.

import Foundation

/// The registered `filing.recent_mailboxes` setting (spec/02).
public enum RecentMailboxes {
    public static let key = "filing.recent_mailboxes"
    public static let limit = 100

    /// The stored ids, newest first; nil for a value outside the
    /// registry's form.
    public static func decode(_ value: String) -> [Int64]? {
        guard !value.isEmpty else { return [] }
        let words = value.components(separatedBy: "\n")
        let ids = words.compactMap { Int64($0) }
        guard ids.count == words.count else { return nil }
        return ids
    }

    public static func encode(_ ids: [Int64]) -> String {
        ids.map(String.init).joined(separator: "\n")
    }

    static func setting(in settings: [Setting]) -> [Int64]? {
        settings.first { $0.key == key }.flatMap { decode($0.value) }
    }

    /// `id` first, once, within the limit.
    static func recording(_ id: Int64, in recent: [Int64]) -> [Int64] {
        Array(([id] + recent.filter { $0 != id }).prefix(limit))
    }
}

public enum FilingDestinations {
    /// The account's Archive folder.
    public static func archive(in mailboxes: [Mailbox]) -> Mailbox? {
        mailboxes.first { $0.kind == .folder && $0.role == .archive }
    }

    /// The folders a selection shown in `shownMailboxId` may move to.
    public static func folders(in mailboxes: [Mailbox], shownMailboxId: Int64?) -> [Mailbox] {
        mailboxes.filter {
            $0.kind == .folder && $0.selectable && $0.role != .sent && $0.role != .drafts
                && $0.mailboxId != shownMailboxId
        }
    }

    /// The account's user labels.
    public static func labels(in mailboxes: [Mailbox]) -> [Mailbox] {
        mailboxes.filter { $0.kind == .label }
    }

    /// What dropping rows on `mailbox` does (spec/08: a folder moves, a
    /// label adds it). The core explains the other kinds' refusals.
    public static func dropAction(
        onto mailbox: Mailbox, shownMailboxId: Int64?
    ) throws(FilingRefusal) -> FilingAction {
        guard mailbox.mailboxId != shownMailboxId else {
            throw .alreadyThere(mailbox: mailbox.name)
        }
        if mailbox.kind == .label { return .addLabel(mailbox) }
        guard mailbox.selectable else { throw .holdsNoMessages(mailbox: mailbox.name) }
        return .move(to: mailbox)
    }

    /// The mailboxes whose names contain every word of `query`,
    /// ignoring case and diacritics: recent ones first, newest first,
    /// then the order they were given in.
    public static func ranked(_ mailboxes: [Mailbox], recent: [Int64], query: String) -> [Mailbox] {
        let words = query.split(whereSeparator: \.isWhitespace).map(String.init)
        let rank = Dictionary(
            recent.enumerated().map { ($0.element, $0.offset) },
            uniquingKeysWith: { first, _ in first })
        return mailboxes.enumerated()
            .filter { _, mailbox in
                words.allSatisfy {
                    mailbox.name.range(of: $0, options: [.caseInsensitive, .diacriticInsensitive])
                        != nil
                }
            }
            .sorted { left, right in
                let leftRank = rank[left.element.mailboxId] ?? Int.max
                let rightRank = rank[right.element.mailboxId] ?? Int.max
                return (leftRank, left.offset) < (rightRank, right.offset)
            }
            .map(\.element)
    }
}
