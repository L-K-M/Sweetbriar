// The coverage note under Search results (spec/08 "Search"): what a
// page's status — or standalone coverage, while Search can't open —
// says about how much of the scope Search could see, and what the
// indexer is doing. Pure text, free of AppKit, so the bridge's Linux
// tests check it.
//
//   312 messages not yet searchable · Indexing in progress   [Details]
//   ─────────────────────────────────────────────────────────────────
//   Indexed: 1,234 of 1,546          Headers: 1,546 of 1,546
//   Partly indexed: 1 of 1,546       Message bodies: 1,500 of 1,546
//   …                                Attachment contents are not …

import Foundation

/// The note's lines for one coverage snapshot. Partial and failed
/// documents count as not searchable: only `indexed` is complete.
public struct SearchCoverageNote: Sendable, Equatable {
    /// The v1 limitation line, shown even when indexing is complete.
    public static let limitations = "Attachment contents are not searched; CJK word search is limited"

    /// One line: how much of the scope is searchable, unknown
    /// exclusions, and the indexer's state when it isn't idle.
    public let summary: String

    /// Every count over its denominator, then the limitations.
    public let details: [String]

    /// The note for a Search page's status.
    public init(status: SearchStatus, locale: Locale = .current, timeZone: TimeZone = .current) {
        self.init(
            coverage: status.coverage, excludedUnknown: status.excludedUnknown,
            locale: locale, timeZone: timeZone
        )
    }

    /// - Parameters:
    ///   - excludedUnknown: the query's unknown exclusions; zero for
    ///     standalone coverage, which has no query.
    ///   - locale: how counts and times are written; tests pin one.
    ///   - timeZone: where the next retry time is told; tests pin one.
    public init(
        coverage: SearchCoverage, excludedUnknown: UInt64,
        locale: Locale = .current, timeZone: TimeZone = .current
    ) {
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
        if excludedUnknown > 0 {
            parts.append("\(messages(excludedUnknown)) couldn't be checked")
        }
        let time = { (utcMillis: Int64) in
            Date(timeIntervalSince1970: Double(utcMillis) / 1_000).formatted(
                Date.FormatStyle(date: .omitted, time: .standard, locale: locale, timeZone: timeZone)
            )
        }
        if let state = Self.state(coverage, time: time) {
            parts.append(state)
        }
        summary = parts.joined(separator: " · ")

        let of = { (n: UInt64) in "\(count(n)) of \(count(coverage.totalMessages))" }
        // Rebuild progress is global: the committed count of its phase
        // and every pending document, not the scope's queue.
        let rebuild = coverage.activeRebuild.map {
            "Rebuild: \(Self.phase($0.phase)), \(count($0.processed)) done, "
                + "\(count($0.pendingDocuments)) documents pending"
        }
        details = (rebuild.map { [$0] } ?? []) + [
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

    /// The indexer's state, when it isn't idle: a rebuild's phase, a
    /// paused rebuild until Resume (also after relaunch), an I/O pause's
    /// error and next automatic retry — never a spinner — and the app
    /// update a blocked indexer needs (spec/08).
    private static func state(_ coverage: SearchCoverage, time: (Int64) -> String) -> String? {
        let job = coverage.activeRebuild
        switch coverage.indexState {
        case .ready:
            return job.map { "Rebuilding the index: \(phase($0.phase))" }
        case .running:
            return job.map { "Rebuilding the index: \(phase($0.phase))" } ?? "Indexing in progress"
        case .pausedByUser:
            return job.map { "Rebuild paused while \(phase($0.phase)); Resume to continue" }
                ?? "Indexing paused"
        case .pausedIo(_, let message, let retryAt):
            let sentence = message.hasSuffix(".") ? message : message + "."
            return "Indexing paused: \(sentence) Next retry at \(time(retryAt))."
        case .blockedNewerExtractor:
            return "Search needs a newer version of Sweetbriar. Update the app to search this archive."
        }
    }

    /// A rebuild phase as the note tells it.
    private static func phase(_ phase: SearchRebuildPhase) -> String {
        switch phase {
        case .reset: return "resetting"
        case .enqueue: return "queueing messages"
        case .index: return "indexing"
        case .complete: return "finishing"
        }
    }
}
