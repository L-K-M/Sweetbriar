import Foundation
import Testing

@testable import SweetbriarBridge

@Suite(.timeLimit(.minutes(1)))
@MainActor
struct ActivityModelTests {
    @Test
    func subscribeComesBeforeTheFirstRead() async throws {
        let source = FakeActivitySource()
        let model = ActivityModel(source: source)
        model.start()
        try #require(await eventually { await source.calls.count == 4 })
        #expect(
            await source.calls == [
                .subscribe,
                .accountHealth,
                .intents(IntentFilter(accountId: nil, state: nil), nil, 500),
                .recovery(nil, nil, 500),
            ])
        model.stop()
    }

    @Test
    func aChangeRereadsEverySection() async throws {
        let source = FakeActivitySource()
        let model = ActivityModel(source: source)
        model.start()
        try #require(await eventually { await source.calls.count == 4 })
        await source.setHealth([activityHealthRow()])
        await source.setIntents(
            IntentPage(rows: [activityIntentRow()], after: "cursor", hasMore: true))
        await source.setRecovery(
            RecoveryPage(rows: [activityRecoveryRow()], after: "cursor", hasMore: false))
        let feed = try #require(await source.feeds.first)
        await feed.emit()
        try #require(await eventually { await model.accounts.count == 1 })
        #expect(model.intents.count == 1)
        #expect(model.intentsTruncated)
        #expect(model.recovery.count == 1)
        #expect(!model.recoveryTruncated)
        let reads = await source.healthReads
        let calls = await source.calls
        #expect(reads == 2, "reads=\(reads) calls=\(calls)")
        model.stop()
    }

    @Test
    func changesDuringAReadRunExactlyOneMoreRead() async throws {
        let source = FakeActivitySource()
        let gate = Gate()
        await source.holdHealth(gate)
        let model = ActivityModel(source: source)
        model.start()
        try #require(await eventually { await source.healthReads == 1 })
        let feed = try #require(await source.feeds.first)
        await feed.emit()
        await feed.emit()
        await gate.open()
        try #require(await eventually { await source.healthReads == 2 })
        try? await Task.sleep(for: .milliseconds(100))
        #expect(await source.healthReads == 2, "two changes during one read are one answer")
        model.stop()
    }

    @Test
    func aFailedSectionKeepsTheOthersAndTheErrorIsVisible() async throws {
        let source = FakeActivitySource()
        await source.setHealth([activityHealthRow()])
        await source.setIntents(IntentPage(rows: [activityIntentRow()], after: nil, hasMore: false))
        let model = ActivityModel(source: source)
        model.start()
        try #require(await eventually { await source.calls.count == 4 })
        await source.setHealth([activityHealthRow("acct-2")])
        await source.failIntents(CoreFailure.unavailable(reason: "intents unavailable"))
        let feed = try #require(await source.feeds.first)
        await feed.emit()
        try #require(await eventually { await model.loadError != nil })
        #expect(model.accounts.count == 1 && model.accounts[0].accountId == "acct-2")
        #expect(model.intents.count == 1, "a failed section keeps the last good answer")
        #expect(
            model.loadError as? CoreFailure
                == .unavailable(reason: "intents unavailable"))
        await source.failIntents(nil)
        await feed.emit()
        try #require(await eventually { await model.loadError == nil })
        #expect(model.intents.count == 1)
        model.stop()
    }

    @Test
    func stopCancelsTheFollower() async throws {
        let source = FakeActivitySource()
        let model = ActivityModel(source: source)
        model.start()
        try #require(await eventually { await source.calls.count == 4 })
        model.stop()
        let feed = try #require(await source.feeds.first)
        await feed.emit()
        try? await Task.sleep(for: .milliseconds(100))
        #expect(await source.healthReads == 1)
    }

    @Test
    func aStoppedFollowersLateFailureLeavesTheRestartedOneAlone() async throws {
        let source = FakeActivitySource()
        let gate = Gate()
        await source.holdSubscribe(gate)
        let model = ActivityModel(source: source)
        model.start()
        try #require(await eventually { await source.subscribeCount == 1 })
        model.stop()
        await source.holdSubscribe(nil)
        model.start()
        try #require(await eventually { await source.feeds.count == 1 })
        // Both subscribes, then the restarted follower's full read.
        try #require(await eventually { await source.calls.count == 5 })
        try? await Task.sleep(for: .milliseconds(50))
        await source.failSubscribe(CoreFailure.unavailable(reason: "store closing"))
        await gate.open()
        try? await Task.sleep(for: .milliseconds(100))
        #expect(model.loadError == nil, "the stale follower's failure is not the restarted one's")
        model.stop()
    }

    @Test
    func resolvePassesAFreshKeyPerActionAndGuardsTheInFlightEvent() async throws {
        let source = FakeActivitySource()
        let gate = Gate()
        await source.holdResolve(gate)
        let model = ActivityModel(source: source)
        model.resolve(eventId: "event-1", resolution: .accept)
        #expect(model.resolving == ["event-1"])
        model.resolve(eventId: "event-1", resolution: .keep)
        model.resolve(eventId: "event-2", resolution: .keep)
        try #require(await eventually { await source.resolveCalls.count == 2 })
        await gate.open()
        try #require(await eventually { await model.resolving.isEmpty })
        let calls = await source.resolveCalls
        #expect(calls.count == 2)
        #expect(calls[0].eventId == "event-1" && calls[0].resolution == .accept)
        #expect(calls[1].eventId == "event-2" && calls[1].resolution == .keep)
        #expect(calls[0].key != calls[1].key)
        #expect(calls.allSatisfy { UUID(uuidString: $0.key) != nil })
        #expect(
            await eventually { await source.healthReads == 2 },
            "each success re-reads account health")
        #expect(model.actionError == nil)
        model.stop()
    }

    @Test
    func aFailedResolveSetsAnErrorAndTheNextSuccessClearsIt() async throws {
        let source = FakeActivitySource()
        let model = ActivityModel(source: source)
        await source.failResolve(CoreFailure.notFound(what: "breaker event"))
        model.resolve(eventId: "event-1", resolution: .accept)
        try #require(await eventually { await model.actionError != nil })
        #expect(model.resolving.isEmpty)
        #expect(model.actionError as? CoreFailure == .notFound(what: "breaker event"))
        await source.failResolve(nil)
        model.resolve(eventId: "event-2", resolution: .keep)
        try #require(await eventually { await model.actionError == nil })
        #expect(model.resolving.isEmpty)
        model.stop()
    }

    @Test
    func aSubscriptionErrorSetsTheErrorAndRetryRestarts() async throws {
        let source = FakeActivitySource()
        let model = ActivityModel(source: source)
        let failure = CoreFailure.unavailable(reason: "health unavailable")
        await source.failSubscribe(failure)
        model.start()
        try #require(await eventually { await model.loadError != nil })
        #expect(model.loadError as? CoreFailure == failure)
        #expect(await source.healthReads == 0)
        #expect(await source.subscribeCount == 1)
        #expect(await source.calls == [.subscribe])
        await source.failSubscribe(nil)
        model.retry()
        try #require(await eventually { await source.healthReads == 1 })
        #expect(model.loadError == nil)
        #expect(await source.subscribeCount == 2)
        #expect(Array(await source.calls).prefix(3) == [.subscribe, .subscribe, .accountHealth])
        model.stop()
    }

    @Test
    func aResolveAfterASubscriptionErrorKeepsTheErrorAndRetry() async throws {
        let source = FakeActivitySource()
        let model = ActivityModel(source: source)
        let failure = CoreFailure.unavailable(reason: "health unavailable")
        await source.failSubscribe(failure)
        model.start()
        try #require(await eventually { await model.loadError != nil })
        // The resolve's own health re-read succeeds, but the window is
        // still not following: the error stays and retry still works.
        model.resolve(eventId: "event-1", resolution: .accept)
        try #require(await eventually { await source.healthReads == 1 })
        try #require(await eventually { await model.resolving.isEmpty })
        #expect(model.loadError as? CoreFailure == failure)
        await source.failSubscribe(nil)
        model.retry()
        try #require(await eventually { await source.subscribeCount == 2 })
        try #require(await eventually { await source.healthReads == 2 })
        model.stop()
    }

    @Test
    func aReopenDuringARunningReadStillReads() async throws {
        let source = FakeActivitySource()
        let gate = Gate()
        await source.setHealth([activityHealthRow()])
        await source.holdHealth(gate)
        let model = ActivityModel(source: source)
        model.start()
        try #require(await eventually { await source.healthReads == 1 })
        // The held read ignores cancellation and finishes after the
        // window reopens; the new follower must still read afresh.
        model.stop()
        model.start()
        try #require(await eventually { await source.subscribeCount == 2 })
        await gate.open()
        try #require(await eventually { await model.accounts == [activityHealthRow()] })
        #expect(await source.healthReads == 2)
        model.stop()
    }
}
