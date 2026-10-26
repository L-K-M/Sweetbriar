// `IndexControlModel`: the Search index controls as the app drives them
// (spec/08 "Search"): Rebuild, Cancel and Resume on the durable rebuild
// job, Retry now during an I/O pause, and standalone coverage, polled
// while a reset or enqueue keeps Search from opening. Free of AppKit so
// the bridge's Linux tests exercise it.
//
//   shown coverage ─► IndexControls ─► rebuild │ cancel │ resume │ retry
//   (window status            ▲                         │
//    or standalone)           └── one refresh per command
//
// Closing a window stops the polling only: the job is durable and
// never cancelled by the UI going away (spec/08).

import Observation

/// The index commands and the standalone coverage query the controls
/// need from the core (spec/07). `CoreBridge` adopts it; tests
/// substitute a fake.
public protocol SearchIndexSource: Sendable {
    func coverage(scope: SearchScope) async throws -> SearchCoverage
    func startSearchRebuild() async throws -> SearchRebuildStarted
    func resumeSearchRebuild(jobId: String) async throws -> Receipt
    func retrySearchIndex() async throws -> Receipt
    func cancelJob(jobId: String) async throws -> Receipt
}

extension CoreBridge: SearchIndexSource {}

/// The controls one coverage snapshot offers (spec/08): Rebuild while
/// no job is active, Cancel while one runs, Resume once the user paused
/// it, Retry now during an I/O pause. A blocked indexer offers none: it
/// needs a newer app, not a command.
public struct IndexControls: OptionSet, Sendable, Hashable {
    public let rawValue: Int

    public init(rawValue: Int) {
        self.rawValue = rawValue
    }

    public static let rebuild = IndexControls(rawValue: 1 << 0)
    public static let cancel = IndexControls(rawValue: 1 << 1)
    public static let resume = IndexControls(rawValue: 1 << 2)
    public static let retryNow = IndexControls(rawValue: 1 << 3)

    public init(coverage: SearchCoverage) {
        let active = coverage.activeRebuild != nil
        switch coverage.indexState {
        case .blockedNewerExtractor:
            self = []
        case .pausedByUser:
            self = active ? .resume : .rebuild
        case .pausedIo:
            self = active ? [.cancel, .retryNow] : [.rebuild, .retryNow]
        case .running, .ready:
            self = active ? .cancel : .rebuild
        }
    }
}

extension SearchCoverage {
    /// The rebuild still under way — paused or not; nil once complete.
    public var activeRebuild: SearchRebuildProgress? {
        guard let rebuild, rebuild.phase != .complete else { return nil }
        return rebuild
    }

    /// Whether a Search window can open on this snapshot: not blocked
    /// on a newer extractor, and no rebuild resetting or requeueing the
    /// index — paused there or not (spec/07).
    public var searchCanOpen: Bool {
        if case .blockedNewerExtractor = indexState { return false }
        switch activeRebuild?.phase {
        case .reset, .enqueue: return false
        case .index, .complete, nil: return true
        }
    }
}

@MainActor
@Observable
public final class IndexControlModel {
    /// How often standalone coverage is polled while Search can't open.
    public static let defaultPollInterval: Duration = .milliseconds(250)

    /// One index command.
    public enum Command: Sendable, Equatable {
        case rebuild, cancel, resume, retryNow

        /// The control this command needs offered.
        var control: IndexControls {
            switch self {
            case .rebuild: return .rebuild
            case .cancel: return .cancel
            case .resume: return .resume
            case .retryNow: return .retryNow
            }
        }
    }

    /// The newest standalone coverage read, if any.
    public private(set) var coverage: SearchCoverage?

    /// The command waiting for its receipt and the refresh after it;
    /// every control is disabled meanwhile.
    public private(set) var pending: Command?

    /// The last command's failure, until the next command succeeds.
    public private(set) var commandError: (any Error)?

    /// The last coverage read's failure, until a read succeeds. Polling
    /// goes on, keeping the last coverage.
    public private(set) var readError: (any Error)?

    @ObservationIgnored
    private var polling: Task<Void, Never>?
    /// A stopped poll loop, still unwinding: the next loop waits for it,
    /// so polls never overlap.
    @ObservationIgnored
    private var retiring: Task<Void, Never>?
    @ObservationIgnored
    private var scope: SearchScope = .all

    private let source: any SearchIndexSource
    private let pollInterval: Duration

    public init(
        source: any SearchIndexSource,
        pollInterval: Duration = IndexControlModel.defaultPollInterval
    ) {
        self.source = source
        self.pollInterval = pollInterval
    }

    /// The coverage controls act on: the newer of a window's status and
    /// the standalone read, by Search generation.
    public func shown(with window: SearchCoverage?) -> SearchCoverage? {
        guard let window, let coverage else { return window ?? coverage }
        return coverage.generation > window.generation ? coverage : window
    }

    /// Run `command` on the job `shown` names, if `shown` offers it,
    /// then read coverage once so the controls follow at once rather
    /// than at the next invalidation. One command at a time.
    public func perform(_ command: Command, on shown: SearchCoverage, scope: SearchScope) async {
        guard pending == nil, IndexControls(coverage: shown).contains(command.control) else { return }
        pending = command
        defer { pending = nil }
        self.scope = scope
        do {
            switch command {
            case .rebuild:
                _ = try await source.startSearchRebuild()
            case .cancel:
                guard let job = shown.activeRebuild else { return }
                _ = try await source.cancelJob(jobId: job.jobId)
            case .resume:
                guard let job = shown.activeRebuild else { return }
                _ = try await source.resumeSearchRebuild(jobId: job.jobId)
            case .retryNow:
                _ = try await source.retrySearchIndex()
            }
            commandError = nil
        } catch is CancellationError {
            return
        } catch {
            commandError = error
            return
        }
        await refresh()
    }

    /// Poll standalone coverage of `scope` with one request in flight,
    /// until `stopPolling`. Starting again only changes the scope.
    public func startPolling(scope: SearchScope) {
        self.scope = scope
        guard polling == nil else { return }
        let interval = pollInterval
        let previous = retiring
        polling = Task { [weak self] in
            await previous?.value
            while !Task.isCancelled {
                guard let self else { return }
                await self.refresh()
                try? await Task.sleep(for: interval)
            }
        }
    }

    /// Stop polling; the job is left as it is.
    public func stopPolling() {
        polling?.cancel()
        retiring = polling ?? retiring
        polling = nil
    }

    /// Whether standalone coverage is being polled.
    public var isPolling: Bool { polling != nil }

    /// Read standalone coverage once. A read never replaces a newer
    /// one: a command's refresh and a poll can cross.
    private func refresh() async {
        do {
            let value = try await source.coverage(scope: scope)
            readError = nil
            if let coverage, coverage.generation > value.generation { return }
            coverage = value
        } catch is CancellationError {
        } catch {
            readError = error
        }
    }
}
