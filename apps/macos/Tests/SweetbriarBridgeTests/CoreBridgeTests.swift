// The bridge's contract (spec/07): settings round-trip through the
// real core, a cancelled call throws `CancellationError` with no
// effect, and calls after shutdown answer `unavailable`.

import Foundation
import Testing
@testable import SweetbriarBridge

@Test
func settingRoundTripThenShutdownAnswersUnavailable() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    _ = try await bridge.setSetting(key: "appearance.compose_cue", value: "strong")

    let settings = try await bridge.settings()
    let cue = try #require(settings.first { $0.key == "appearance.compose_cue" })
    #expect(cue.value == "strong")
    #expect(!cue.isDefault)

    await bridge.shutdown()
    do {
        _ = try await bridge.settings()
        Issue.record("a call after shutdown must throw")
    } catch let failure as CoreFailure {
        guard case .unavailable = failure else {
            Issue.record("expected .unavailable, got \(failure)")
            return
        }
    }
}

/// The sidebar queries cross the bridge (spec/07): a fresh core lists
/// its local archive with no mailboxes yet, and an unknown account is
/// `notFound`.
@Test
func aFreshCoreListsItsLocalArchive() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let accounts = try await bridge.accounts()
    #expect(accounts.count == 1)
    let archive = try #require(accounts.first)
    #expect(archive.kind == .local)
    #expect(archive.state == .active)

    let list = try await bridge.mailboxes(accountId: archive.accountId)
    #expect(list.mailboxes.isEmpty)

    do {
        _ = try await bridge.mailboxes(accountId: "no-such-account")
        Issue.record("an unknown account must throw")
    } catch let failure as CoreFailure {
        guard case .notFound = failure else {
            Issue.record("expected .notFound, got \(failure)")
            return
        }
    }

    await bridge.shutdown()
}

/// Every store spec opens over the bridge; over an empty store each
/// is empty and knows it (spec/07 "Store windows").
@Test
func storeWindowsOpenOverTheBridge() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let specs: [WindowSpec] = [.mailbox(1), .unified(.inbox), .flagged, .thread(1)]
    for spec in specs {
        let window = try await bridge.openWindow(spec: spec, anchor: .top, limit: 10)
        #expect(window.rows.isEmpty, "\(spec)")
        #expect(window.total == .exact(n: 0), "\(spec)")
        await bridge.closeWindow(id: window.windowId)
    }

    await bridge.shutdown()
}

/// A subscription crosses the bridge (spec/07 "Subscriptions"): a fill
/// invalidates the window at its new generation, and a pending `next`
/// ends with `unavailable` once the core shuts down.
@Test(.timeLimit(.minutes(1)))
func aSubscriptionReportsAFillAndEndsAtShutdown() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let window = try await bridge.openWindow(spec: .debugSynthetic, anchor: .top, limit: 10)
    let subscription = try await bridge.subscribe(windowId: window.windowId)

    let generation = try await bridge.debugFillSynthetic(count: 5)
    #expect(try await subscription.next() == .invalidated(generation: generation))

    let pending = Task { try await subscription.next() }
    await bridge.shutdown()
    switch await pending.result {
    case .success(let event):
        Issue.record("expected .unavailable, got \(event)")
    case .failure(let error):
        guard case .unavailable = error as? CoreFailure else {
            Issue.record("expected .unavailable, got \(error)")
            return
        }
    }
}

/// The import commands cross the bridge (spec/07, 09): a started job
/// can be cancelled, a cancelled job can't be confirmed, and an
/// unknown job is `notFound`.
@Test(.timeLimit(.minutes(1)))
func importCommandsCrossTheBridge() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let source = dir.appendingPathComponent("Inbox.mbox")
    try Data("From a@example.test Sat Oct 17 12:00:00 2026\nSubject: one\n\nbody\n".utf8)
        .write(to: source)

    let started = try await bridge.startImport(paths: [source.path])
    #expect(started.receipt.command == "start_import")
    let cancelled = try await bridge.cancelJob(jobId: started.jobId)
    #expect(cancelled.command == "cancel_job")

    do {
        try await bridge.confirmImport(jobId: started.jobId, variants: [])
        Issue.record("a cancelled job must not confirm")
    } catch let failure as CoreFailure {
        guard case .conflict = failure else {
            Issue.record("expected .conflict, got \(failure)")
            return
        }
    }
    do {
        try await bridge.cancelJob(jobId: "no-such-job")
        Issue.record("an unknown job must throw")
    } catch let failure as CoreFailure {
        guard case .notFound = failure else {
            Issue.record("expected .notFound, got \(failure)")
            return
        }
    }

    await bridge.shutdown()
}

/// Poll `importJob` until `done` accepts it, within a bound.
private func waitForJob(
    _ bridge: CoreBridge,
    id: String,
    until done: (ImportJob) -> Bool
) async throws -> ImportJob {
    let deadline = ContinuousClock.now + .seconds(50)
    while true {
        let job = try await bridge.importJob(id: id)
        if done(job) {
            return job
        }
        guard ContinuousClock.now < deadline else {
            throw CoreFailure.internalError(detail: "the job never settled: \(job.state)")
        }
        try await Task.sleep(for: .milliseconds(20))
    }
}

/// An import over the bridge (spec/09): the inventory shows the
/// ambiguous source before confirmation, the job completes with its
/// totals, and its report is published at the path the job names.
@Test(.timeLimit(.minutes(1)))
func anImportRunsOverTheBridge() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let source = dir.appendingPathComponent("Inbox.mbox")
    let mbox = "From a@example.test Sat Oct 17 12:00:00 2026\nSubject: one\n\n>>From here\n"
    try Data(mbox.utf8).write(to: source)

    let started = try await bridge.startImport(paths: [source.path])
    let waiting = try await waitForJob(bridge, id: started.jobId) { $0.state != .inventory }
    #expect(waiting.state == .awaitingConfirmation)
    do {
        _ = try await bridge.importReport(jobId: started.jobId)
        Issue.record("an unfinished job has no report")
    } catch let failure as CoreFailure {
        guard case .conflict = failure else {
            Issue.record("expected .conflict, got \(failure)")
            return
        }
    }
    let inventoried = try #require(waiting.sources.first)
    #expect(waiting.sources.count == 1)
    #expect(inventoried.mailbox == "Inbox")
    #expect(inventoried.kind == .mbox)
    #expect(inventoried.messages == 1)
    #expect(inventoried.needsVariant)

    try await bridge.confirmImport(
        jobId: started.jobId,
        variants: [VariantChoice(ordinal: inventoried.ordinal, variant: .mboxrd)]
    )
    // A finished state alone doesn't mean the report exists.
    let done = try await waitForJob(bridge, id: started.jobId) {
        $0.state.isFinished && $0.reportPath != nil
    }
    #expect(done.state == .completed)
    let report = try #require(try await bridge.importReport(jobId: started.jobId))
    #expect(report.path == done.reportPath)
    #expect(report.json.contains("\"state\": \"completed\""))
    #expect(done.sources.first?.variant == .mboxrd)
    #expect(done.totals.imported == 1)
    #expect(done.problemCount == 0)

    await bridge.shutdown()
}

/// An imported mailbox crosses the bridge field by field (spec/07
/// `mailboxes`): distinct total and unread counts, at the generation
/// its window shows.
@Test(.timeLimit(.minutes(1)))
func anImportedMailboxCrossesTheBridge() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let source = dir.appendingPathComponent("Inbox.mbox")
    let mbox = "From a@example.test Sat Oct 17 12:00:00 2026\nStatus: RO\nSubject: Read\n\nx\n"
        + "From a@example.test Sat Oct 17 12:00:00 2026\nStatus: O\nSubject: Unread\n\ny\n"
    try Data(mbox.utf8).write(to: source)

    let started = try await bridge.startImport(paths: [source.path])
    let deadline = ContinuousClock.now + .seconds(50)
    var confirmed = false
    while true {
        let job = try await bridge.importJob(id: started.jobId)
        if job.state == .awaitingConfirmation, !confirmed {
            try await bridge.confirmImport(jobId: started.jobId, variants: [])
            confirmed = true
        }
        if job.state == .completed {
            break
        }
        try #require(!job.state.isFinished, "the import ended \(job.state)")
        try #require(ContinuousClock.now < deadline, "the import never finished")
        try await Task.sleep(for: .milliseconds(20))
    }

    let archive = try #require(try await bridge.accounts().first)
    #expect(archive.displayName == "Local Archive")
    #expect(archive.stateReason == nil)
    #expect(archive.sortOrder == 0)
    let list = try await bridge.mailboxes(accountId: archive.accountId)
    let inbox = try #require(list.mailboxes.first)
    #expect(list.mailboxes.count == 1)
    #expect(inbox.accountId == archive.accountId)
    #expect(inbox.name == "Inbox")
    #expect(inbox.delimiter == nil)
    #expect(inbox.role == nil)
    #expect(inbox.kind == .folder)
    #expect(inbox.selectable)
    #expect(inbox.total == 2)
    #expect(inbox.unread == 1)
    let window = try await bridge.openWindow(spec: .mailbox(inbox.mailboxId), anchor: .top, limit: 10)
    #expect(list.generation == window.generation)
    #expect(window.rows.count == 2)

    await bridge.shutdown()
}

/// Import `paths` and wait for the finished job, confirming with no
/// variants.
private func importAndFinish(_ bridge: CoreBridge, paths: [String]) async throws -> ImportJob {
    let started = try await bridge.startImport(paths: paths)
    let deadline = ContinuousClock.now + .seconds(50)
    var confirmed = false
    while true {
        let job = try await bridge.importJob(id: started.jobId)
        if job.state == .awaitingConfirmation, !confirmed {
            try await bridge.confirmImport(jobId: started.jobId, variants: [])
            confirmed = true
        }
        if job.state.isFinished {
            return job
        }
        try #require(ContinuousClock.now < deadline, "the import never finished")
        try await Task.sleep(for: .milliseconds(20))
    }
}

/// A job's totals, sources and problems cross the bridge as
/// themselves: a re-import of the same mailbox counts duplicates
/// apart from imports, and a missing path is a problem with its path
/// and reason (spec/09).
@Test(.timeLimit(.minutes(1)))
func importTotalsAndProblemsCrossTheBridge() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let source = dir.appendingPathComponent("Inbox.mbox")
    let mbox = "From a@example.test Sat Oct 17 12:00:00 2026\nStatus: RO\nSubject: One\n\nx\n"
        + "From a@example.test Sat Oct 17 12:00:00 2026\nStatus: O\nSubject: Two\n\ny\n"
        + "From a@example.test Sat Oct 17 12:00:00 2026\nSubject: Three\n\nz\n"
    try Data(mbox.utf8).write(to: source)

    let first = try await importAndFinish(bridge, paths: [source.path])
    #expect(first.state == .completed)
    #expect(first.totals.processed == 3)
    #expect(first.totals.imported == 3)
    #expect(first.totals.duplicates == 0)
    #expect(first.totals.readFromHeaders == 2)
    #expect(first.totals.readDefault == 1)
    let inventoried = try #require(first.sources.first)
    // The core records canonical paths: on macOS /var is /private/var.
    #expect(
        URL(fileURLWithPath: inventoried.path).resolvingSymlinksInPath()
            == source.resolvingSymlinksInPath()
    )
    #expect(inventoried.bytes == UInt64(mbox.utf8.count))
    #expect(inventoried.sha256.count == 64)
    #expect(inventoried.messagesDone == 3)
    #expect(first.failure == nil)

    let missing = dir.appendingPathComponent("missing.mbox").path
    let second = try await importAndFinish(bridge, paths: [source.path, missing])
    #expect(second.totals.processed == 3)
    #expect(second.totals.imported == 0)
    #expect(second.totals.duplicates == 3)
    #expect(second.problemCount == 1)
    let problem = try #require(second.problems.first)
    #expect(problem.path.hasSuffix("/missing.mbox"))
    #expect(problem.kind == .inaccessible)
    #expect(!problem.detail.isEmpty)

    await bridge.shutdown()
}

@Test(.timeLimit(.minutes(1)))
func aCallOnACancelledTaskThrowsAndWritesNothing() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )

    // The task parks on the gate, is cancelled, then runs the call:
    // it's already cancelled when `setSetting` installs its handler,
    // so `onCancel` fires the token before the command can begin.
    let gate = Gate()
    let task = Task {
        await gate.wait()
        return try await bridge.setSetting(
            key: "appearance.compose_cue",
            value: "strong"
        )
    }
    task.cancel()
    await gate.open()

    switch await task.result {
    case .success(let receipt):
        Issue.record("expected CancellationError, got \(receipt)")
    case .failure(let error):
        #expect(error is CancellationError, "expected CancellationError, got \(error)")
    }

    let settings = try await bridge.settings()
    let cue = try #require(settings.first { $0.key == "appearance.compose_cue" })
    #expect(cue.isDefault)

    await bridge.shutdown()
}

/// Cancelling a task whose call is already in flight cancels the
/// call's token (spec/07): the work waits for the token the way a
/// query waits in the core, so only `onCancel` can release it.
@Test(.timeLimit(.minutes(1)))
func cancellingACallInFlightCancelsItsToken() async throws {
    let token = RecordingToken()
    let entered = Gate()
    let task = Task {
        try await CoreBridge.withCancellation(token: token) { () async throws -> Bool in
            await entered.open()
            guard await eventually({ token.isCancelled }) else {
                return false
            }
            throw CancellationError()
        }
    }

    await entered.wait()
    #expect(!token.isCancelled, "the token was cancelled before the task was")
    task.cancel()

    let result = await task.result
    #expect(token.isCancelled, "cancelling the task didn't cancel the call's token")
    guard case .failure(let error) = result else {
        Issue.record("the call ran on without its token being cancelled")
        return
    }
    #expect(error is CancellationError, "expected CancellationError, got \(error)")
}

/// A shutdown that arrives while the open is still running waits for
/// it and closes the core it produced (spec/07 "Lifecycle"): the
/// session's core then answers `unavailable`, and its lease is gone —
/// the store opens again.
@Test(.timeLimit(.minutes(1)))
func aShutdownDuringTheOpenClosesWhatTheOpenProduced() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let sink = await sinkBatcher(clock: ManualFrameClock())

    let session = CoreSession {
        try await CoreBridge.open(config: config, snapshots: sink)
    }
    await session.shutdown()

    let opened = try await session.bridge()
    do {
        _ = try await opened.settings()
        Issue.record("the session's core still answered after shutdown")
    } catch let failure as CoreFailure {
        guard case .unavailable = failure else {
            Issue.record("expected .unavailable, got \(failure)")
            return
        }
    }

    let reopened = try await CoreBridge.open(config: config, snapshots: sink)
    await reopened.shutdown()
}

/// Reader metadata crosses the bridge from a real import and selected occurrence.
@Test(.timeLimit(.minutes(1)))
func readerMetadataCrossesTheBridge() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let source = dir.appendingPathComponent("Reader.mbox")
    let mbox = "From sender@example.test Sat Oct 17 12:00:00 2026\n"
        + "From: Sender <sender@example.test>\nTo: Recipient <to@example.test>\n"
        + "Cc: Copy <cc@example.test>\nDate: Sat, 17 Oct 2026 12:00:00 +0000\n"
        + "Subject: Reader subject\nMessage-ID: <reader@example.test>\n\nbody\n"
    try Data(mbox.utf8).write(to: source)
    let job = try await importAndFinish(bridge, paths: [source.path])
    #expect(job.state == .completed)
    let archive = try #require(try await bridge.accounts().first)
    let mailbox = try #require(try await bridge.mailboxes(accountId: archive.accountId).mailboxes.first)
    let window = try await bridge.openWindow(spec: .mailbox(mailbox.mailboxId), anchor: .top, limit: 10)
    #expect(window.rows.count == 1)
    let row = try #require(window.rows.first)
    let id = try await bridge.messageForOccurrence(occurrenceId: row.occurrenceId)
    #expect(id == row.messageId)
    var message = try await bridge.getMessage(messageId: id)
    let deadline = ContinuousClock.now + .seconds(30)
    while message.threadId == nil {
        try #require(ContinuousClock.now < deadline, "threads were never built")
        try await Task.sleep(for: .milliseconds(20))
        message = try await bridge.getMessage(messageId: id)
    }
    let threadId = try #require(message.threadId)
    let thread = try await bridge.openWindow(spec: .thread(threadId), anchor: .top, limit: 10)
    #expect(thread.rows.map(\.messageId) == [id])
    #expect(message == ReaderMessage(
        messageId: id,
        headers: ReaderHeaders(
            from: [ReaderAddress(name: "Sender", address: "sender@example.test")],
            to: [ReaderAddress(name: "Recipient", address: "to@example.test")],
            cc: [ReaderAddress(name: "Copy", address: "cc@example.test")],
            date: 1_792_238_400_000, subject: "Reader subject", messageId: "reader@example.test"
        ),
        accountId: archive.accountId, accountDisplayName: "Local Archive",
        occurrences: [ReaderOccurrence(
            occurrenceId: row.occurrenceId, mailboxId: mailbox.mailboxId, mailboxName: "Reader"
        )],
        threadId: threadId, tags: [], note: nil, bodyAvailability: .acquired
    ))
    for lookup in [true, false] {
        do {
            if lookup { _ = try await bridge.messageForOccurrence(occurrenceId: -1) }
            else { _ = try await bridge.getMessage(messageId: -1) }
            Issue.record("an unknown id must throw")
        } catch let failure as CoreFailure {
            guard case .notFound = failure else {
                Issue.record("expected .notFound, got \(failure)")
                continue
            }
        }
    }
    await bridge.closeWindow(id: window.windowId)
    await bridge.closeWindow(id: thread.windowId)
    await bridge.shutdown()
}
