// The coverage note's text (spec/08 "Search"): unsearchable counts
// with partial and failed as incomplete, unknown exclusions, an empty
// scope without a percentage, the indexer's state — a rebuild's phase
// and global progress, a paused rebuild, an I/O pause's error and next
// retry, a blocked indexer's update — every count over its
// denominator, and the limitations line even when complete.

import Foundation
import Testing
@testable import SweetbriarBridge

private let english = Locale(identifier: "en_US")
private let utc = TimeZone(identifier: "UTC")!

private func status(
    total: UInt64 = 1_546, indexed: UInt64 = 1_546, partial: UInt64 = 0, failed: UInt64 = 0,
    queue: UInt64 = 0, state: SearchIndexState = .ready, unknown: UInt64 = 0,
    rebuild: SearchRebuildProgress? = nil
) -> SearchStatus {
    SearchStatus(
        coverage: SearchCoverage(
            generation: 7, extractorVersion: 1, totalMessages: total,
            headersSynced: total, bodiesAcquired: total - min(total, 46),
            indexed: indexed, partial: partial, failed: failed,
            pending: total - indexed - partial - failed, queueLength: queue,
            indexState: state, rebuild: rebuild, limitations: [.cjkUnsegmented, .attachmentTextUnavailable]
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

/// An error without its own full stop still reads as two sentences.
@Test
func anIoPauseMessageEndsBeforeTheRetryTime() {
    let state = SearchIndexState.pausedIo(code: .otherIo, message: "Disk full", retryAt: 0)
    let note = SearchCoverageNote(status: status(state: state), locale: english, timeZone: utc)
    #expect(note.summary.contains("Indexing paused: Disk full. Next retry at "))
    let urgent = SearchIndexState.pausedIo(code: .otherIo, message: "Disk full!", retryAt: 0)
    let exclaimed = SearchCoverageNote(status: status(state: urgent), locale: english, timeZone: utc)
    #expect(exclaimed.summary.contains("Indexing paused: Disk full! Next retry at "))
}

@Test(arguments: [
    (SearchIndexState.pausedByUser, "Indexing paused"),
    (.blockedNewerExtractor, "Search needs a newer version of Sweetbriar. Update the app to search this archive."),
])
func theIndexerStateJoinsTheSummary(_ state: SearchIndexState, _ text: String) {
    let note = SearchCoverageNote(status: status(state: state), locale: english)
    #expect(note.summary == "1,546 messages indexed · \(text)")
}

/// An I/O pause shows its actionable error and the next automatic
/// retry, told in the given time zone — not a spinner.
@Test
func anIoPauseShowsItsErrorAndNextRetry() {
    let state = SearchIndexState.pausedIo(code: .storageFull, message: "Free up disk space.", retryAt: 3_723_000)
    let note = SearchCoverageNote(status: status(state: state), locale: Locale(identifier: "en_GB"), timeZone: utc)
    // Platforms differ on a leading zero for the hour.
    #expect(note.summary.hasPrefix("1,546 messages indexed · Indexing paused: Free up disk space. Next retry at "))
    #expect(note.summary.hasSuffix("1:02:03."))
}

/// A rebuild shows its phase in the summary and, in the details, the
/// phase's committed count and the global pending documents — not the
/// scope's queue (spec/08). A cancelled rebuild reads paused until
/// Resume; a finished one adds no line.
@Test(arguments: [
    (SearchIndexState.running, SearchRebuildPhase.reset, "Rebuilding the index: resetting", "resetting"),
    (.running, .enqueue, "Rebuilding the index: queueing messages", "queueing messages"),
    (.running, .index, "Rebuilding the index: indexing", "indexing"),
    (.pausedByUser, .reset, "Rebuild paused while resetting; Resume to continue", "resetting"),
    (.pausedByUser, .enqueue, "Rebuild paused while queueing messages; Resume to continue", "queueing messages"),
    (.pausedByUser, .index, "Rebuild paused while indexing; Resume to continue", "indexing"),
])
func aRebuildShowsItsPhaseAndGlobalProgress(
    _ state: SearchIndexState, _ phase: SearchRebuildPhase, _ summary: String, _ named: String
) {
    let job = SearchRebuildProgress(jobId: "job", phase: phase, processed: 1_200, pendingDocuments: 3_400)
    let note = SearchCoverageNote(status: status(queue: 9, state: state, rebuild: job), locale: english)
    #expect(note.summary == "1,546 messages indexed · \(summary)")
    #expect(note.details.first == "Rebuild: \(named), 1,200 done, 3,400 documents pending")
    #expect(note.details.contains("Queued for indexing: 9 of 1,546"))
}

@Test
func aFinishedRebuildAddsNothing() {
    let job = SearchRebuildProgress(jobId: "job", phase: .complete, processed: 1_546, pendingDocuments: 0)
    let note = SearchCoverageNote(status: status(rebuild: job), locale: english)
    #expect(note.summary == "1,546 messages indexed")
    #expect(note.details == SearchCoverageNote(status: status(), locale: english).details)
}

/// Standalone coverage has no query: no unknown exclusions.
@Test
func standaloneCoverageReadsLikeAStatusWithoutExclusions() {
    let page = status(indexed: 1_000, unknown: 0)
    #expect(SearchCoverageNote(coverage: page.coverage, excludedUnknown: 0, locale: english)
        == SearchCoverageNote(status: page, locale: english))
}
