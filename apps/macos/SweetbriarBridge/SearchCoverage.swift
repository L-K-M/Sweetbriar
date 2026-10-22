// Owned coverage values: the public bridge never exports generated SDK types.
import SweetbriarCore

public enum SearchScope: Sendable, Equatable, Hashable {
    case all
    case account(String)
    case mailbox(Int64)

    var generated: SweetbriarCore.SearchScope {
        switch self {
        case .all: return .all
        case .account(let id): return .account(accountId: id)
        case .mailbox(let id): return .mailbox(mailboxId: id)
        }
    }
}

/// Scoped message facts and global operational state from one native snapshot.
public struct SearchCoverage: Sendable, Equatable {
    public var generation: UInt64
    public var extractorVersion: UInt32
    public var totalMessages: UInt64
    public var headersSynced: UInt64
    public var bodiesAcquired: UInt64
    public var indexed: UInt64
    public var partial: UInt64
    public var failed: UInt64
    public var pending: UInt64
    public var queueLength: UInt64
    public var indexState: SearchIndexState
    public var rebuild: SearchRebuildProgress?
    public var limitations: [SearchLimitation]

    public init(
        generation: UInt64, extractorVersion: UInt32, totalMessages: UInt64,
        headersSynced: UInt64, bodiesAcquired: UInt64, indexed: UInt64,
        partial: UInt64, failed: UInt64, pending: UInt64, queueLength: UInt64,
        indexState: SearchIndexState, rebuild: SearchRebuildProgress?, limitations: [SearchLimitation]
    ) {
        self.generation = generation
        self.extractorVersion = extractorVersion
        self.totalMessages = totalMessages
        self.headersSynced = headersSynced
        self.bodiesAcquired = bodiesAcquired
        self.indexed = indexed
        self.partial = partial
        self.failed = failed
        self.pending = pending
        self.queueLength = queueLength
        self.indexState = indexState
        self.rebuild = rebuild
        self.limitations = limitations
    }

    init(_ value: SweetbriarCore.Coverage) {
        self.init(
            generation: value.generation, extractorVersion: value.extractorVersion,
            totalMessages: value.totalMessages, headersSynced: value.headersSynced,
            bodiesAcquired: value.bodiesAcquired, indexed: value.indexed,
            partial: value.partial, failed: value.failed, pending: value.pending,
            queueLength: value.queueLength, indexState: SearchIndexState(value.indexState),
            rebuild: value.rebuild.map(SearchRebuildProgress.init),
            limitations: value.limitations.map(SearchLimitation.init)
        )
    }
}

public enum SearchIndexState: Sendable, Equatable {
    case ready
    case running
    case pausedByUser
    /// `retryAt` is UTC milliseconds, not a sentinel or a duration.
    case pausedIo(code: SearchIoCode, message: String, retryAt: Int64)
    case blockedNewerExtractor

    init(_ value: SweetbriarCore.SearchIndexState) {
        switch value {
        case .ready: self = .ready
        case .running: self = .running
        case .pausedByUser: self = .pausedByUser
        case .blockedNewerExtractor: self = .blockedNewerExtractor
        case .pausedIo(let code, let message, let retryAt):
            self = .pausedIo(code: SearchIoCode(code), message: message, retryAt: retryAt)
        }
    }
}

public enum SearchIoCode: Sendable, Equatable {
    case storageFull, permissionDenied, storageUnavailable, otherIo

    init(_ value: SweetbriarCore.SearchIoCode) {
        switch value {
        case .storageFull: self = .storageFull
        case .permissionDenied: self = .permissionDenied
        case .storageUnavailable: self = .storageUnavailable
        case .otherIo: self = .otherIo
        }
    }
}

public struct SearchRebuildProgress: Sendable, Equatable {
    public var jobId: String
    public var phase: SearchRebuildPhase
    public var processed: UInt64
    public var pendingDocuments: UInt64

    public init(jobId: String, phase: SearchRebuildPhase, processed: UInt64, pendingDocuments: UInt64) {
        self.jobId = jobId
        self.phase = phase
        self.processed = processed
        self.pendingDocuments = pendingDocuments
    }

    init(_ value: SweetbriarCore.SearchRebuildProgress) {
        self.init(jobId: value.jobId, phase: SearchRebuildPhase(value.phase),
                  processed: value.processed, pendingDocuments: value.pendingDocuments)
    }
}

public enum SearchRebuildPhase: Sendable, Equatable {
    case reset, enqueue, index, complete

    init(_ value: SweetbriarCore.SearchRebuildPhase) {
        switch value {
        case .reset: self = .reset
        case .enqueue: self = .enqueue
        case .index: self = .index
        case .complete: self = .complete
        }
    }
}

public enum SearchLimitation: Sendable, Equatable {
    case cjkUnsegmented, attachmentTextUnavailable

    init(_ value: SweetbriarCore.SearchLimitation) {
        switch value {
        case .cjkUnsegmented: self = .cjkUnsegmented
        case .attachmentTextUnavailable: self = .attachmentTextUnavailable
        }
    }
}
