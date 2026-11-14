// Shared across Settings and the main-window health banner (spec/08).
// Closing Settings does not cancel work. The app awaits cancelAndWait before
// shutting down its session; begun native restores must return their outcome.

import Foundation
import Observation

@MainActor
@Observable
public final class BackupsModel {
    /// Each query is a whole answer, independent of the other sections.
    public enum ReadState: Sendable {
        case unloaded, loaded
        case failed(any Error)
    }

    public enum Activity: Sendable, Equatable {
        case backingUp, savingDestination
        case restoring(generationId: String)
        case cancellingRestore(generationId: String)
    }

    public enum ActionResult: Sendable, Equatable {
        case backup(generationId: String, end: BackupEnd)
        case backupReplayed(generationId: String)
        case backupWaitCancelled(generationId: String)
        case restore(RestoreOutcome)
        case requestCancelled
        case destinationSaved
    }

    public private(set) var generations: [BackupGeneration] = []
    public private(set) var artifacts: [RestoreArtifact] = []
    public private(set) var generationsReadState: ReadState = .unloaded
    public private(set) var artifactsReadState: ReadState = .unloaded
    public private(set) var destinationError: (any Error)?
    public private(set) var destination: String?
    public var destinationDraft = ""
    public var selectedGenerationId: String?
    /// A value snapshot, not a live selection that can change under confirmation.
    public private(set) var confirmation: BackupGeneration?
    public private(set) var health: Health?
    public private(set) var activity: Activity?
    public private(set) var lastResult: ActionResult?
    public private(set) var commandError: (any Error)?
    public private(set) var readError: (any Error)?
    public private(set) var healthError: (any Error)?
    public private(set) var isRefreshing = false
    public private(set) var isClosed = false
    /// A shell-local invalidation, independent of decreasing store generations.
    public private(set) var archiveRevision: UInt64 = 0

    private let source: any BackupsSource
    private let prepareRestore: @MainActor @Sendable () async -> Void
    private let didKeepArchive: @MainActor @Sendable () async -> Void
    private let didRestore: @MainActor @Sendable () async -> Void
    private var stopped = false
    private var readTicket: UInt64 = 0
    private var healthTicket: UInt64 = 0
    private var watchTicket: UInt64 = 0
    @ObservationIgnored private var operation: Task<Void, Never>?
    @ObservationIgnored private var reading: Task<Void, Never>?
    @ObservationIgnored private var following: Task<Void, Never>?

    private static let destinationKey = "backups.destination"
    private enum DraftRefresh { case preserveEdits, replace }

    public init(
        source: any BackupsSource,
        prepareRestore: @escaping @MainActor @Sendable () async -> Void = {},
        didKeepArchive: @escaping @MainActor @Sendable () async -> Void = {},
        didRestore: @escaping @MainActor @Sendable () async -> Void = {}
    ) {
        self.source = source
        self.prepareRestore = prepareRestore
        self.didKeepArchive = didKeepArchive
        self.didRestore = didRestore
    }

    deinit {
        operation?.cancel()
        reading?.cancel()
        following?.cancel()
    }

    public var isBusy: Bool { activity != nil }
    public var canWrite: Bool {
        guard !stopped, !isClosed, !isBusy, healthError == nil, let health else { return false }
        return health.integrity == .sound
    }
    public var canSaveDestination: Bool { canWrite && destination != nil && destinationError == nil }
    public var canRestore: Bool {
        !stopped && !isClosed && !isBusy
            && generations.contains { $0.id == selectedGenerationId && $0.state == .complete }
    }

    /// Start the long-lived watch even while Settings is hidden. Subscribe
    /// before the first read, and never retain the model across next().
    public func start() {
        guard !stopped, !isClosed, following == nil else { return }
        followHealth()
    }

    /// Refresh is owned by the shared model, not by a transient Settings task.
    public func refresh() async {
        guard !stopped, !isClosed else { return }
        if healthError != nil { followHealth() } else { start() }
        await beginRead(.preserveEdits).value
    }

    public func requestRestore() {
        guard canRestore else { return }
        confirmation = generations.first { $0.id == selectedGenerationId && $0.state == .complete }
    }

    public func dismissConfirmation() { confirmation = nil }

    public func confirmRestore() {
        guard !stopped, !isClosed, !isBusy, let chosen = confirmation else { return }
        confirmation = nil
        activity = .restoring(generationId: chosen.id)
        commandError = nil
        lastResult = nil
        operation = Task { [weak self] in await self?.restore(chosen.id) }
    }

    /// Request cancellation, but keep awaiting the native outcome. A flag may
    /// arrive after the swap; Restored still invalidates the replaced content.
    public func cancelRestore() {
        guard case .restoring(let id) = activity else { return }
        activity = .cancellingRestore(generationId: id)
        operation?.cancel()
    }

    public func backUpNow() {
        guard canWrite else { return }
        activity = .backingUp
        commandError = nil
        lastResult = nil
        operation = Task { [weak self] in await self?.backup() }
    }

    public func saveDestination() {
        guard canSaveDestination else { return }
        let value = destinationDraft
        activity = .savingDestination
        commandError = nil
        lastResult = nil
        operation = Task { [weak self] in await self?.save(value) }
    }

    /// The quit path calls this before CoreSession.shutdown. Await the command
    /// first: even a cancelled restore may install a core and start a fresh read.
    public func cancelAndWait() async {
        stopped = true
        operation?.cancel()
        reading?.cancel()
        following?.cancel()
        await operation?.value
        await reading?.value
        await following?.value
    }

    private func backup() async {
        defer {
            activity = nil
            operation = nil
        }
        do {
            let started = try await source.startBackup(idempotencyKey: UUID().uuidString)
            if let copy = started.copy {
                do {
                    lastResult = .backup(generationId: started.generationId, end: try await copy.finished())
                } catch is CancellationError {
                    lastResult = .backupWaitCancelled(generationId: started.generationId)
                }
            } else {
                // A replay may name an unfinished/failed/retained-away generation.
                lastResult = .backupReplayed(generationId: started.generationId)
            }
        } catch is CancellationError {
            lastResult = .requestCancelled
        } catch {
            commandError = error
        }
        await beginRead(.preserveEdits).value
    }

    private func save(_ value: String) async {
        defer {
            activity = nil
            operation = nil
        }
        do {
            _ = try await source.setSetting(key: Self.destinationKey, value: value)
            destination = value
            lastResult = .destinationSaved
        } catch is CancellationError {
            lastResult = .requestCancelled
        } catch {
            commandError = error
        }
        await beginRead(.preserveEdits).value
    }

    private func restore(_ id: String) async {
        defer {
            activity = nil
            operation = nil
        }
        do {
            await prepareRestore()
            try Task.checkCancellation()
            let outcome = try await source.startRestore(idempotencyKey: UUID().uuidString, generationId: id)
            // Deliberately no Task.isCancelled check: an acknowledged swap stands.
            lastResult = .restore(outcome)
            switch outcome {
            case .restored, .receiptUnrecorded:
                archiveRevision &+= 1
                readTicket &+= 1
                healthTicket &+= 1
                generations = []
                artifacts = []
                generationsReadState = .unloaded
                artifactsReadState = .unloaded
                selectedGenerationId = nil
                confirmation = nil
                destination = nil
                destinationDraft = ""
                health = nil
                readError = nil
                destinationError = nil
                healthError = nil
                // CoreBridge.settings also publishes to shared observers. Finish
                // pre-swap reads before the shell can open new handles/settings.
                let previousRead = reading
                previousRead?.cancel()
                await previousRead?.value
                // The shell may open new handles after teardown. Run it on a
                // fresh task too: the request's cancellation must not poison them.
                let invalidate = didRestore
                await Task { await invalidate() }.value
                if !stopped { followHealth() }
                await beginRead(.replace).value
            case .replayed, .refused:
                await reopenKeptArchive()
                await beginRead(.preserveEdits).value
            case .closed:
                isClosed = true
                // A fenced read cannot clear progress, and none will replace it.
                isRefreshing = false
                readTicket &+= 1
                healthTicket &+= 1
                reading?.cancel()
                following?.cancel()
            }
        } catch is CancellationError {
            lastResult = .requestCancelled
            await reopenKeptArchive()
            await beginRead(.preserveEdits).value
        } catch {
            commandError = error
            await reopenKeptArchive()
            await beginRead(.preserveEdits).value
        }
    }

    private func reopenKeptArchive() async {
        let reopen = didKeepArchive
        await Task { await reopen() }.value
    }

    /// Unstructured tasks do not inherit the command's cancellation. Retire
    /// reads in order, and fence their publication by both ticket and archive.
    private func beginRead(_ draft: DraftRefresh) -> Task<Void, Never> {
        let previous = reading
        previous?.cancel()
        readTicket &+= 1
        let ticket = readTicket
        let revision = archiveRevision
        isRefreshing = true
        let task = Task { [weak self] in
            await previous?.value
            await self?.read(draft, ticket: ticket, revision: revision)
        }
        reading = task
        return task
    }

    private func read(_ draft: DraftRefresh, ticket: UInt64, revision: UInt64) async {
        defer {
            if ticket == readTicket {
                isRefreshing = false
                reading = nil
            }
        }
        do {
            let source = source
            let rows = try await captureRead { try await source.listBackups() }
            let names = try await captureRead { try await source.listRestoreArtifacts() }
            let setting = try await captureRead {
                let settings = try await source.settings()
                guard let value = settings.first(where: { $0.key == Self.destinationKey })?.value else {
                    throw CoreFailure.internalError(detail: "The backup destination setting is missing.")
                }
                return value
            }
            try Task.checkCancellation()
            guard ticket == readTicket, revision == archiveRevision, !isClosed else { return }
            // Publish once within this epoch, but never let corrupt preferences
            // hide a whole backup-ledger answer needed to recover that store.
            var failure: (any Error)?
            switch rows {
            case .success(let rows):
                generations = rows
                generationsReadState = .loaded
                if !rows.contains(where: { $0.id == selectedGenerationId }) { selectedGenerationId = nil }
            case .failure(let cause):
                generationsReadState = .failed(cause)
                failure = cause
            }
            switch names {
            case .success(let names):
                artifacts = names
                artifactsReadState = .loaded
            case .failure(let cause):
                artifactsReadState = .failed(cause)
                failure = failure ?? cause
            }
            switch setting {
            case .success(let value):
                let edited = destinationDraft != (destination ?? "")
                destination = value
                destinationError = nil
                if case .replace = draft {
                    destinationDraft = value
                } else if !edited {
                    destinationDraft = value
                }
            case .failure(let cause):
                destinationError = cause
                failure = failure ?? cause
            }
            readError = failure
        } catch is CancellationError {
        } catch {
            if ticket == readTicket, revision == archiveRevision, !isClosed { readError = error }
        }
    }

    private func captureRead<Value: Sendable>(
        _ read: @MainActor () async throws -> Value
    ) async throws -> Result<Value, any Error> {
        try Task.checkCancellation()
        do { return .success(try await read()) } catch is CancellationError {
            throw CancellationError()
        } catch { return .failure(error) }
    }

    private func followHealth() {
        let previous = following
        previous?.cancel()
        watchTicket &+= 1
        let ticket = watchTicket
        let source = source
        following = Task { [weak self] in
            await previous?.value
            defer {
                if self?.watchTicket == ticket { self?.following = nil }
            }
            while !Task.isCancelled {
                do {
                    let changes = try await source.subscribeHealth()
                    await self?.readHealth()
                    while !Task.isCancelled {
                        try await changes.next()
                        await self?.readHealth()
                    }
                } catch is CancellationError {
                    return
                } catch CoreFailure.resetRequired {
                    continue
                } catch {
                    self?.healthError = error
                    return
                }
            }
        }
    }

    private func readHealth() async {
        healthTicket &+= 1
        let ticket = healthTicket
        let revision = archiveRevision
        do {
            let value = try await source.health()
            guard ticket == healthTicket, revision == archiveRevision, !isClosed else { return }
            health = value
            healthError = nil
        } catch is CancellationError {
        } catch {
            if ticket == healthTicket, revision == archiveRevision, !isClosed { healthError = error }
        }
    }
}
