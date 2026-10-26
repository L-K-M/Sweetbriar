// The coverage note's text (spec/08 "Search"): unsearchable counts
// with partial and failed as incomplete, unknown exclusions, an empty
// scope without a percentage, the indexer's state, every count over
// its denominator, and the limitations line even when complete.

import Foundation
import Testing
@testable import SweetbriarBridge

private let english = Locale(identifier: "en_US")

private func status(
    total: UInt64 = 1_546, indexed: UInt64 = 1_546, partial: UInt64 = 0, failed: UInt64 = 0,
    queue: UInt64 = 0, state: SearchIndexState = .ready, unknown: UInt64 = 0
) -> SearchStatus {
    SearchStatus(
        coverage: SearchCoverage(
            generation: 7, extractorVersion: 1, totalMessages: total,
            headersSynced: total, bodiesAcquired: total - min(total, 46),
            indexed: indexed, partial: partial, failed: failed,
            pending: total - indexed - partial - failed, queueLength: queue,
            indexState: state, rebuild: nil, limitations: [.cjkUnsegmented, .attachmentTextUnavailable]
        ),
        excludedUnknown: unknown
    )
}

@Test
func completeCoverageStillListsTheLimitations() {
    let note = SearchCoverageNote(status: status(), locale: english)
    #expect(note.summary == "1,546 messages indexed")
    #expect(note.details == [
        "Indexed: 1,546 of 1,546",
        "Partly indexed: 0 of 1,546",
        "Failed: 0 of 1,546",
        "Pending: 0 of 1,546",
        "Headers: 1,546 of 1,546",
        "Message bodies: 1,500 of 1,546",
        "Queued for indexing: 0 of 1,546",
        "Attachment contents are not searched; CJK word search is limited",
    ])
}

@Test
func partialFailedAndPendingAreNotYetSearchable() {
    let note = SearchCoverageNote(
        status: status(indexed: 1_234, partial: 1, failed: 2, queue: 309, state: .running),
        locale: english
    )
    #expect(note.summary == "312 messages not yet searchable · Indexing in progress")
    #expect(note.details.prefix(4) == [
        "Indexed: 1,234 of 1,546",
        "Partly indexed: 1 of 1,546",
        "Failed: 2 of 1,546",
        "Pending: 309 of 1,546",
    ])
    #expect(note.details[6] == "Queued for indexing: 309 of 1,546")
}

@Test
func unknownExclusionsShowWhenNonzero() {
    let one = SearchCoverageNote(status: status(indexed: 1_545, partial: 1, unknown: 1), locale: english)
    #expect(one.summary == "1 message not yet searchable · 1 message couldn't be checked")
    let many = SearchCoverageNote(status: status(unknown: 3), locale: english)
    #expect(many.summary == "1,546 messages indexed · 3 messages couldn't be checked")
}

@Test
func anEmptyScopeShowsZeroWithoutAPercentage() {
    let note = SearchCoverageNote(status: status(total: 0, indexed: 0), locale: english)
    #expect(note.summary == "No messages in this scope")
    #expect(note.details.first == "Indexed: 0 of 0")
    #expect(!note.summary.contains("%") && !note.details.contains { $0.contains("%") })
}

@Test(arguments: [
    (SearchIndexState.pausedByUser, "Indexing paused"),
    (.pausedIo(code: .storageFull, message: "Free up disk space", retryAt: 1_000), "Indexing paused: Free up disk space"),
    (.blockedNewerExtractor, "Search needs a newer version of Sweetbriar"),
])
func theIndexerStateJoinsTheSummary(_ state: SearchIndexState, _ text: String) {
    let note = SearchCoverageNote(status: status(state: state), locale: english)
    #expect(note.summary == "1,546 messages indexed · \(text)")
}
