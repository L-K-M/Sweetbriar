// The Activity window's model (spec/08 "Activity / account health";
// spec/07): per-account health, parked and quarantined intents and the
// Recovery list, following `subscribe_account_health`. Closing the
// window cancels the loop; the app tears the core down separately.

import Foundation
import Observation

/// The Activity model's owned bridge surface. Tests supply a scripted
/// source; the app uses its existing CoreBridge, never a second core.
public protocol ActivitySource: Sendable {
    /// Named apart from `BackupsSource.HealthEvents`: one CoreBridge
    /// conforms to both, and two same-named associated types would
    /// fight over the same method.
    associatedtype AccountHealthEvents: ActivityHealthEvents

    func accountHealth() async throws -> [AccountHealth]
    func subscribeAccountHealth() async throws -> AccountHealthEvents
    func intents(filter: IntentFilter, after: String?, limit: UInt32) async throws -> IntentPage
    func recovery(accountId: String?, after: String?, limit: UInt32) async throws -> RecoveryPage
    func resolveBreakerEvent(
        idempotencyKey: String, eventId: String, resolution: BreakerResolution
    ) async throws -> Receipt
}

/// One in-flight next per stream; every answer requires a new read.
public protocol ActivityHealthEvents: Sendable {
    func next() async throws
}

extension CoreBridge: ActivitySource {}
extension AccountHealthSubscription: ActivityHealthEvents {}

/// The Activity window's state: one full read of the three sections,
/// refreshed as `account_health` changes. Reads coalesce — a change
/// during a read runs one more read after it, never one per change.
@MainActor
@Observable
public final class ActivityModel {
    public private(set) var accounts: [AccountHealth] = []
    public private(set) var intents: [Intent] = []
    /// More intents follow the first page; the window offers no older
    /// page until M3.1, so the flag is what keeps the list honest.
    public private(set) var intentsTruncated = false
    public private(set) var recovery: [Recovery] = []
    public private(set) var recoveryTruncated = false
    /// The last full read's first failure, or nil; the sections keep
    /// what they showed before.
    public private(set) var loadError: (any Error)?
    /// The last breaker resolution's failure, cleared by the next
    /// success; nil while none failed.
    public private(set) var actionError: (any Error)?
    /// Event ids with a resolution in flight.
    public private(set) var resolving: Set<String> = []

    private let source: any ActivitySource
    private var healthTicket: UInt64 = 0
    private var readTicket: UInt64 = 0
    private var readPending = false
    private var followTicket: UInt64 = 0
    /// Bumped by `stop()`: a read that outlives its follower must not
    /// clear the restarted one's read or its pending re-read.
    private var readGeneration: UInt64 = 0
    @ObservationIgnored private var reading: Task<Void, Never>?
    @ObservationIgnored private var following: Task<Void, Never>?

    /// The first page's limit: the core's own page ceiling is 1,000.
    private static let pageLimit: UInt32 = 500

    public init(source: any ActivitySource) {
        self.source = source
    }

    deinit {
        reading?.cancel()
        following?.cancel()
    }

    /// Subscribe before the first read, then follow every change.
    public func start() {
        guard following == nil else { return }
        follow()
    }

    /// Cancel the loop. A later `start()` or `retry()` begins again.
    public func stop() {
        let follower = following
        following = nil
        follower?.cancel()
        reading?.cancel()
        reading = nil
        readPending = false
        readGeneration &+= 1
    }

    /// Restart after a subscription error; a failed follower stays
    /// still until this runs. A healthy follower is left alone.
    public func retry() {
        guard loadError != nil || following == nil else { return }
        stop()
        loadError = nil
        follow()
    }

    /// Resolve one breaker event. Each user action takes a fresh key;
    /// a second call for an event already in flight is ignored. A
    /// success clears any previous action error and re-reads health.
    public func resolve(eventId: String, resolution: BreakerResolution) {
        guard !resolving.contains(eventId) else { return }
        resolving.insert(eventId)
        Task { [weak self] in
            let key = UUID().uuidString
            do {
                guard let source = self?.source else { return }
                _ = try await source.resolveBreakerEvent(
                    idempotencyKey: key, eventId: eventId, resolution: resolution
                )
                guard let self else { return }
                self.resolving.remove(eventId)
                self.actionError = nil
                await self.refreshAccounts()
            } catch is CancellationError {
                self?.resolving.remove(eventId)
            } catch {
                guard let self else { return }
                self.resolving.remove(eventId)
                self.actionError = error
            }
        }
    }

    /// Re-read health after a command; a newer read wins. A success
    /// leaves `loadError` alone: only a full read or `retry()` clears
    /// it, so a failed subscription stays visible and retryable.
    private func refreshAccounts() async {
        healthTicket &+= 1
        let ticket = healthTicket
        do {
            let rows = try await source.accountHealth()
            guard ticket == healthTicket else { return }
            accounts = rows
        } catch is CancellationError {
        } catch {
            guard ticket == healthTicket else { return }
            loadError = error
        }
    }

    private func follow() {
        followTicket &+= 1
        let ticket = followTicket
        let source = source
        let task = Task { [weak self] in
            guard let self else { return }
            defer {
                if self.followTicket == ticket {
                    self.following = nil
                }
            }
            do {
                // Subscribe first: a change between the subscription
                // and the first read is caught by the first `next`.
                let changes = try await source.subscribeAccountHealth()
                await self.beginRead().value
                while !Task.isCancelled {
                    try await changes.next()
                    guard !Task.isCancelled else { return }
                    await self.beginRead().value
                }
            } catch is CancellationError {
            } catch {
                // A failed subscription stops following; retry() starts
                // over, and the window keeps its last good state.
                self.loadError = error
            }
        }
        following = task
    }

    /// The read in flight, or one more after it when a change landed
    /// while it ran.
    private func beginRead() -> Task<Void, Never> {
        readTicket &+= 1
        let ticket = readTicket
        if let reading {
            readPending = true
            return reading
        }
        let generation = readGeneration
        let task = Task { [weak self] in
            guard let self else { return }
            await self.read(ticket: ticket, generation: generation)
        }
        reading = task
        return task
    }

    private func read(ticket: UInt64, generation: UInt64) async {
        defer {
            if generation == readGeneration {
                finishRead()
            }
        }
        let accountsTicket = healthTicket
        var failure: (any Error)?
        var newAccounts: [AccountHealth]?
        var newIntents: IntentPage?
        var newRecovery: RecoveryPage?

        do {
            newAccounts = try await source.accountHealth()
        } catch is CancellationError {
            return
        } catch {
            failure = error
        }
        do {
            // The query itself returns parked and quarantined only, so
            // the filter names neither (spec/07).
            newIntents = try await source.intents(
                filter: IntentFilter(accountId: nil, state: nil),
                after: nil, limit: Self.pageLimit
            )
        } catch is CancellationError {
            return
        } catch {
            failure = failure ?? error
        }
        do {
            newRecovery = try await source.recovery(
                accountId: nil, after: nil, limit: Self.pageLimit
            )
        } catch is CancellationError {
            return
        } catch {
            failure = failure ?? error
        }
        guard ticket == readTicket else { return }
        if let newAccounts, accountsTicket == healthTicket { accounts = newAccounts }
        if let newIntents {
            intents = newIntents.rows
            intentsTruncated = newIntents.hasMore
        }
        if let newRecovery {
            recovery = newRecovery.rows
            recoveryTruncated = newRecovery.hasMore
        }
        loadError = failure
    }

    private func finishRead() {
        reading = nil
        if !Task.isCancelled, readPending {
            readPending = false
            _ = beginRead()
        }
    }
}
