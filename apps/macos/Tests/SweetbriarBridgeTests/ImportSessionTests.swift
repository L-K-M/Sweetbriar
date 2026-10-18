// `ImportSession` (spec/09 "In the app"): the inventory before
// confirmation, a variant per ambiguous source, at most one poll in
// flight, cancel waiting for its receipt, detaching without touching
// the job, and a report shown only once written. Plus one run over the
// real core.

import Foundation
import Testing
@testable import SweetbriarBridge

/// A scripted `ImportJobSource`: `importJob` answers the job in the
/// state the script reached; `confirm` and `cancel` move it on. It
/// counts polls and the most in flight at once, and publishes the
/// report only after `reportAfter` looks.
actor FakeImportSource: ImportJobSource {
    private(set) var state: ImportJobState = .inventory
    private var polls = 0
    private(set) var pollCount = 0
    private var inFlight = 0
    private(set) var maxInFlight = 0
    private(set) var confirmed: [VariantChoice]?
    private(set) var cancelCalls = 0
    private(set) var reportLooks = 0
    /// How many report looks find nothing before the report exists;
    /// nil for never.
    private var reportAfter: Int? = 0
    /// Polls in `inventory` before the inventory finishes.
    private let inventoryPolls: Int

    init(inventoryPolls: Int = 2) {
        self.inventoryPolls = inventoryPolls
    }

    func publishReport(after looks: Int?) {
        reportAfter = looks
    }

    func startImport(paths: [String], target: ImportTarget) async throws -> ImportStarted {
        ImportStarted(
            receipt: Receipt(command: "start_import", resultJson: "{}", createdAtMs: 0),
            jobId: "job-1"
        )
    }

    func confirmImport(jobId: String, variants: [VariantChoice]) async throws -> Receipt {
        guard state == .awaitingConfirmation else {
            throw CoreFailure.conflict(detail: "not waiting")
        }
        confirmed = variants
        state = .running
        return Receipt(command: "confirm_import", resultJson: "{}", createdAtMs: 0)
    }

    func cancelJob(jobId: String) async throws -> Receipt {
        cancelCalls += 1
        guard !state.isFinished else {
            throw CoreFailure.conflict(detail: "finished")
        }
        state = .cancelled
        return Receipt(command: "cancel_job", resultJson: "{}", createdAtMs: 0)
    }

    func importJob(id: String) async throws -> ImportJob {
        pollCount += 1
        inFlight += 1
        maxInFlight = max(maxInFlight, inFlight)
        defer { inFlight -= 1 }
        try await Task.sleep(for: .milliseconds(2))
        polls += 1
        if state == .inventory, polls >= inventoryPolls {
            state = .awaitingConfirmation
        } else if state == .running {
            state = .completed
        }
        return job()
    }

    func importReport(jobId: String) async throws -> ImportReport? {
        reportLooks += 1
        guard let reportAfter, reportLooks > reportAfter else { return nil }
        return ImportReport(path: "/tmp/report.json", json: "{\"v\":1}")
    }

    private func job() -> ImportJob {
        let inventoried = state != .inventory
        let source = { (ordinal: UInt64, ambiguous: Bool) in
            ImportSource(
                ordinal: ordinal,
                path: "/tmp/\(ordinal).mbox",
                mailbox: "Box \(ordinal)",
                kind: .mbox,
                messages: 2,
                bytes: 100,
                sha256: "",
                needsVariant: ambiguous,
                variant: nil,
                messagesDone: 0
            )
        }
        return ImportJob(
            id: "job-1",
            state: state,
            sources: inventoried ? [source(0, false), source(1, true), source(2, true)] : [],
            problems: [],
            problemCount: 0,
            totals: ImportTotals(
                processed: 0, imported: 0, duplicates: 0, rejected: 0,
                bytesStored: 0, readFromHeaders: 0, readDefault: 0, flagsIncomplete: 0
            ),
            failure: nil,
            reportPath: state.isFinished ? "/tmp/report.json" : nil
        )
    }
}

private let fastPolls: Duration = .milliseconds(1)

/// The inventory shows before confirmation; confirming needs a variant
/// for each ambiguous source, and sends exactly those; the job then
/// completes and its report is shown once written. One poll at a time.
@MainActor
@Test(.timeLimit(.minutes(1)))
func anImportWaitsForEachAmbiguousSourcesVariant() async throws {
    let fake = FakeImportSource()
    let session = ImportSession(source: fake, pollInterval: fastPolls)
    try await session.start(paths: ["/tmp/0.mbox"])
    try #require(await eventually { await session.job?.state == .awaitingConfirmation })
    #expect(session.ambiguousSources.map(\.ordinal) == [1, 2])
    #expect(!session.canConfirm)

    session.choose(.mboxrd, forSource: 1)
    #expect(!session.canConfirm)
    session.choose(.mboxo, forSource: 2)
    #expect(session.canConfirm)
    try await session.confirm()

    try #require(await eventually {
        let report = await session.report
        return report != .notFinished && report != .pending
    })
    #expect(session.job?.state == .completed)
    #expect(session.report == .written(ImportReport(path: "/tmp/report.json", json: "{\"v\":1}")))
    #expect(await fake.confirmed == [
        VariantChoice(ordinal: 1, variant: .mboxrd),
        VariantChoice(ordinal: 2, variant: .mboxo),
    ])
    #expect(await fake.maxInFlight == 1)
}

/// Cancel waits for `cancel_job`'s receipt, then follows the job to
/// its cancelled end.
@MainActor
@Test(.timeLimit(.minutes(1)))
func cancelWaitsForItsReceipt() async throws {
    let fake = FakeImportSource()
    let session = ImportSession(source: fake, pollInterval: fastPolls)
    try await session.start(paths: ["/tmp/0.mbox"])
    try #require(await eventually { await session.job?.state == .awaitingConfirmation })

    try await session.cancel()
    #expect(await fake.cancelCalls == 1)
    #expect(await fake.state == .cancelled)
    try #require(await eventually { await session.job?.state == .cancelled })
    #expect(!session.canConfirm)
}

/// A report not written yet shows as pending, never as written, and
/// polling for it stops; Retry looks again.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aMissingReportStaysPendingUntilRetried() async throws {
    let fake = FakeImportSource(inventoryPolls: 1)
    await fake.publishReport(after: nil)
    let session = ImportSession(
        source: fake,
        pollInterval: fastPolls,
        reportPatience: .milliseconds(30)
    )
    try await session.start(paths: ["/tmp/0.mbox"])
    try #require(await eventually { await session.job?.state == .awaitingConfirmation })
    session.choose(.mboxrd, forSource: 1)
    session.choose(.mboxrd, forSource: 2)
    try await session.confirm()

    try #require(await eventually { await session.report == .pending })
    // Past the patience, polling stops.
    try await Task.sleep(for: .milliseconds(100))
    let polls = await fake.pollCount
    try await Task.sleep(for: .milliseconds(50))
    #expect(await fake.pollCount == polls, "polling stopped")
    #expect(await fake.reportLooks > 1, "looked more than once")
    #expect(session.report == .pending)

    await fake.publishReport(after: 0)
    session.retryReport()
    try #require(await eventually {
        if case .written = await session.report { return true }
        return false
    })
}

/// Detaching stops the polls and leaves the job alone; attaching again
/// picks it up where it is.
@MainActor
@Test(.timeLimit(.minutes(1)))
func detachLeavesTheJobRunning() async throws {
    let fake = FakeImportSource(inventoryPolls: 1_000)
    let session = ImportSession(source: fake, pollInterval: fastPolls)
    try await session.start(paths: ["/tmp/0.mbox"])
    try #require(await eventually { await fake.pollCount >= 2 })

    session.detach()
    try await Task.sleep(for: .milliseconds(20))
    let polls = await fake.pollCount
    try await Task.sleep(for: .milliseconds(50))
    #expect(await fake.pollCount == polls)
    #expect(await fake.cancelCalls == 0)

    let again = ImportSession(source: fake, pollInterval: fastPolls)
    again.attach(jobId: "job-1")
    try #require(await eventually { await fake.pollCount > polls })
    #expect(again.jobId == "job-1")
    again.detach()
}

/// The whole flow over the real core: an ambiguous source waits for
/// its variant, the job completes, and the report is the published
/// one at the path the job names.
@MainActor
@Test(.timeLimit(.minutes(1)))
func anImportSessionRunsOverTheRealCore() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: sinkBatcher(clock: ManualFrameClock())
    )
    let source = dir.appendingPathComponent("Inbox.mbox")
    try Data("From a@example.test Sat Oct 17 12:00:00 2026\nSubject: q\n\n>>From x\n".utf8)
        .write(to: source)

    let session = ImportSession(source: bridge, pollInterval: .milliseconds(10))
    try await session.start(paths: [source.path])
    try #require(await eventually { await session.job?.state == .awaitingConfirmation })
    let ambiguous = try #require(session.ambiguousSources.first)
    session.choose(.mboxrd, forSource: ambiguous.ordinal)
    try await session.confirm()

    try #require(await eventually {
        if case .written = await session.report { return true }
        return false
    })
    #expect(session.job?.state == .completed)
    guard case .written(let report) = session.report else { return }
    #expect(FileManager.default.fileExists(atPath: report.path))
    #expect(report.json.contains("\"state\": \"completed\""))

    await bridge.shutdown()
}
