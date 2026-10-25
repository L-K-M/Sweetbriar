// The coverage note under Search results (spec/08 "Search"): what a
// page's status says about how much of the scope Search could see.
// Pure text, free of AppKit, so the bridge's Linux tests check it.
//
//   312 messages not yet searchable · Indexing in progress   [Details]
//   ─────────────────────────────────────────────────────────────────
//   Indexed: 1,234 of 1,546          Headers: 1,546 of 1,546
//   Partly indexed: 1 of 1,546       Message bodies: 1,500 of 1,546
//   …                                Attachment contents are not …

import Foundation

/// The note's lines for one `SearchStatus`. Partial and failed
/// documents count as not searchable: only `indexed` is complete.
public struct SearchCoverageNote: Sendable, Equatable {
    /// The v1 limitation line, shown even when indexing is complete.
    public static let limitations = "Attachment contents are not searched; CJK word search is limited"

    /// One line: how much of the scope is searchable, unknown
    /// exclusions, and the indexer's state when it isn't idle.
    public let summary: String

    /// Every count over its denominator, then the limitations.
    public let details: [String]

    /// - Parameter locale: how counts are written; tests pin one.
    public init(status: SearchStatus, locale: Locale = .current) {
        let coverage = status.coverage
        let count = { (n: UInt64) in n.formatted(.number.locale(locale)) }
        let messages = { (n: UInt64) in "\(count(n)) \(n == 1 ? "message" : "messages")" }

        var parts: [String] = []
        let unsearchable = coverage.totalMessages - min(coverage.indexed, coverage.totalMessages)
        if coverage.totalMessages == 0 {
            // An empty scope has nothing to index, not 0% indexed.
            parts.append("No messages in this scope")
        } else if unsearchable > 0 {
            parts.append("\(messages(unsearchable)) not yet searchable")
        } else {
            parts.append("\(messages(coverage.totalMessages)) indexed")
        }
        if status.excludedUnknown > 0 {
            parts.append("\(messages(status.excludedUnknown)) couldn't be checked")
        }
        if let state = Self.state(coverage.indexState) {
            parts.append(state)
        }
        summary = parts.joined(separator: " · ")

        let of = { (n: UInt64) in "\(count(n)) of \(count(coverage.totalMessages))" }
        details = [
            "Indexed: \(of(coverage.indexed))",
            "Partly indexed: \(of(coverage.partial))",
            "Failed: \(of(coverage.failed))",
            "Pending: \(of(coverage.pending))",
            "Headers: \(of(coverage.headersSynced))",
            "Message bodies: \(of(coverage.bodiesAcquired))",
            "Queued for indexing: \(of(coverage.queueLength))",
            Self.limitations,
        ]
    }

    /// The indexer's state, when it isn't idle. Phase progress, the
    /// retry deadline and the controls belong to the index controls.
    private static func state(_ state: SearchIndexState) -> String? {
        switch state {
        case .ready:
            return nil
        case .running:
            return "Indexing in progress"
        case .pausedByUser:
            return "Indexing paused"
        case .pausedIo(_, let message, _):
            return "Indexing paused: \(message)"
        case .blockedNewerExtractor:
            return "Search needs a newer version of Sweetbriar"
        }
    }
}
