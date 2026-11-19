import Foundation

@testable import SweetbriarBridge

/// One change per generation, like the core's subscription: several
/// emissions while no `next` is parked are one answer.
actor FakeActivityHealthEvents: ActivityHealthEvents {
    private var changes: UInt64 = 0
    private var answered: UInt64 = 0
    private var waiter: CheckedContinuation<Void, any Error>?
    private var failure: (any Error)?
    /// A cancellation whose hop reached the actor before `park`'s did:
    /// the two hops race, and the parked continuation must not leak.
    private var cancelledBeforePark = false

    func next() async throws {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation {
                (continuation: CheckedContinuation<Void, any Error>) in
                Task { [weak self] in await self?.park(continuation) }
            }
        } onCancel: {
            Task { [weak self] in await self?.cancelPark() }
        }
    }

    private func park(_ continuation: CheckedContinuation<Void, any Error>) {
        if cancelledBeforePark {
            cancelledBeforePark = false
            continuation.resume(throwing: CancellationError())
            return
        }
        if let failure {
            continuation.resume(throwing: failure)
            return
        }
        if answered != changes {
            answered = changes
            continuation.resume()
            return
        }
        waiter = continuation
    }

    private func cancelPark() {
        guard let waiter else {
            cancelledBeforePark = true
            return
        }
        self.waiter = nil
        waiter.resume(throwing: CancellationError())
    }

    func emit() {
        changes &+= 1
        // The resumed answer covers every change up to now; later ones
        // wait for the next `next`.
        if let waiter = resumeWaiter() {
            answered = changes
            waiter.resume()
        }
    }

    func fail(_ error: any Error) {
        failure = error
        resumeWaiter()?.resume(throwing: error)
    }

    private func resumeWaiter() -> CheckedContinuation<Void, any Error>? {
        let waiter = waiter
        self.waiter = nil
        return waiter
    }
}

actor FakeActivitySource: ActivitySource {
    enum Call: Equatable {
        case accountHealth
        case intents(IntentFilter, String?, UInt32)
        case recovery(String?, String?, UInt32)
        case subscribe
        case resolve(String, String, BreakerResolution)
    }

    struct ResolveCall: Equatable {
        var key: String
        var eventId: String
        var resolution: BreakerResolution
    }

    private(set) var calls: [Call] = []
    private(set) var feeds: [FakeActivityHealthEvents] = []
    private var health: [AccountHealth] = []
    private var intentsPage = IntentPage(rows: [], after: nil, hasMore: false)
    private var recoveryPage = RecoveryPage(rows: [], after: nil, hasMore: false)
    private var healthGate: Gate?
    private var subscribeGate: Gate?
    private var resolveGate: Gate?
    private var intentsFailure: (any Error)?
    private var subscribeFailure: (any Error)?
    private var resolveFailure: (any Error)?
    private var receipt = Receipt(
        command: "resolve_breaker_event", resultJson: "{}", createdAtMs: 1_001)

    func setHealth(_ rows: [AccountHealth]) { health = rows }
    func setIntents(_ page: IntentPage) { intentsPage = page }
    func setRecovery(_ page: RecoveryPage) { recoveryPage = page }
    func holdHealth(_ gate: Gate?) { healthGate = gate }
    func holdResolve(_ gate: Gate?) { resolveGate = gate }
    func holdSubscribe(_ gate: Gate?) { subscribeGate = gate }
    func failIntents(_ error: (any Error)?) { intentsFailure = error }
    func failSubscribe(_ error: (any Error)?) { subscribeFailure = error }
    func failResolve(_ error: (any Error)?) { resolveFailure = error }

    var healthReads: Int { calls.filter { $0 == .accountHealth }.count }
    /// Every subscribe attempt, including one that failed: a failed
    /// subscription leaves no feed behind.
    var subscribeCount: Int { calls.filter { $0 == .subscribe }.count }
    var resolveCalls: [ResolveCall] {
        calls.compactMap { call in
            if case .resolve(let key, let eventId, let resolution) = call {
                return ResolveCall(key: key, eventId: eventId, resolution: resolution)
            }
            return nil
        }
    }

    func accountHealth() async throws -> [AccountHealth] {
        calls.append(.accountHealth)
        let rows = health
        if let healthGate { await healthGate.wait() }
        return rows
    }

    func subscribeAccountHealth() async throws -> FakeActivityHealthEvents {
        calls.append(.subscribe)
        if let subscribeGate { await subscribeGate.wait() }
        if let subscribeFailure { throw subscribeFailure }
        let feed = FakeActivityHealthEvents()
        feeds.append(feed)
        return feed
    }

    func intents(filter: IntentFilter, after: String?, limit: UInt32) async throws -> IntentPage {
        calls.append(.intents(filter, after, limit))
        if let intentsFailure { throw intentsFailure }
        return intentsPage
    }

    func recovery(accountId: String?, after: String?, limit: UInt32) async throws -> RecoveryPage {
        calls.append(.recovery(accountId, after, limit))
        return recoveryPage
    }

    func resolveBreakerEvent(
        idempotencyKey: String, eventId: String, resolution: BreakerResolution
    ) async throws -> Receipt {
        calls.append(.resolve(idempotencyKey, eventId, resolution))
        if let resolveFailure { throw resolveFailure }
        let value = receipt
        if let resolveGate { await resolveGate.wait() }
        return value
    }
}

func activityHealthRow(
    _ id: String = "acct-1", state: AccountState = .active,
    condition: AccountCondition = .healthy,
    events: [AccountBreakerEvent] = []
) -> AccountHealth {
    AccountHealth(
        accountId: id, state: state, stateReason: nil, condition: condition,
        warnings: [], breakerEvents: events,
        coverage: SearchCoverage(
            generation: 0, extractorVersion: 1, totalMessages: 12,
            headersSynced: 12, bodiesAcquired: 12, indexed: 12, partial: 0,
            failed: 0, pending: 0, queueLength: 0, indexState: .ready,
            rebuild: nil, limitations: []
        ),
        lastSync: nil
    )
}

func activityBreakerEvent(
    _ id: String = "event-1", disappeared: UInt64 = 1
) -> AccountBreakerEvent {
    AccountBreakerEvent(
        eventId: id, mailboxId: nil, disappeared: disappeared,
        detectedAt: Date(timeIntervalSince1970: 0)
    )
}

func activityIntentRow(
    _ id: String = "intent-1", kind: IntentKind = .move,
    state: IntentState = .parked, reason: QuarantineReason? = nil,
    lastError: String? = nil
) -> Intent {
    Intent(
        intentId: id, accountId: "acct-1", kind: kind, state: state,
        targetOccurrenceIds: [1], quarantineReason: reason, lastError: lastError,
        effect: .toMailbox(mailboxId: 2), createdAt: Date(timeIntervalSince1970: 1_000)
    )
}

func activityRecoveryRow(
    _ id: Int64 = 1, state: RecoveryState = .vanished
) -> Recovery {
    Recovery(
        messageId: id, accountId: "acct-1", mailboxId: 2, mailboxName: "Inbox",
        state: state, lastSeenAt: Date(timeIntervalSince1970: 1_000), occurrenceId: 3
    )
}
