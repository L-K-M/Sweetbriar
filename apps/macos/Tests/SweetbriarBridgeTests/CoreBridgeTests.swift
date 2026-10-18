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

/// Poll `importJob` until `done` accepts its state, within a bound.
private func waitForJob(
    _ bridge: CoreBridge,
    id: String,
    until done: (ImportJobState) -> Bool
) async throws -> ImportJob {
    let deadline = ContinuousClock.now + .seconds(50)
    while true {
        let job = try await bridge.importJob(id: id)
        if done(job.state) {
            return job
        }
        guard ContinuousClock.now < deadline else {
            throw CoreFailure.internalError(detail: "the job never settled: \(job.state)")
        }
        try await Task.sleep(for: .milliseconds(20))
    }
}

/// An import over the bridge (spec/09): the inventory shows the
/// ambiguous source before confirmation, and the job completes with
/// its totals.
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
    let waiting = try await waitForJob(bridge, id: started.jobId) { $0 != .inventory }
    #expect(waiting.state == .awaitingConfirmation)
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
    let done = try await waitForJob(bridge, id: started.jobId) { $0.isFinished }
    #expect(done.state == .completed)
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
