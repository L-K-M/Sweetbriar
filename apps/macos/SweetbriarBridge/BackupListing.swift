// Bridge-owned backup rows and retained names (spec/07; M1.8j2).
// Listing grants no authority to delete, reuse or restore a path.

import SweetbriarCore

public enum BackupState: Sendable, Equatable {
    case inProgress, complete, failed

    init(_ value: SweetbriarCore.BackupState) {
        switch value {
        case .inProgress: self = .inProgress
        case .complete: self = .complete
        case .failed: self = .failed
        }
    }
}

public enum BackupVerdict: Sendable, Equatable {
    case intact, damaged, missing

    init(_ value: SweetbriarCore.BackupVerdict) {
        switch value {
        case .intact: self = .intact
        case .damaged: self = .damaged
        case .missing: self = .missing
        }
    }
}

public struct BackupVerification: Sendable, Equatable {
    public var verdict: BackupVerdict
    public var checkedAtMs: Int64

    public init(verdict: BackupVerdict, checkedAtMs: Int64) {
        self.verdict = verdict
        self.checkedAtMs = checkedAtMs
    }

    init(_ value: SweetbriarCore.BackupVerification) {
        self.init(verdict: BackupVerdict(value.verdict), checkedAtMs: value.checkedAtMs)
    }
}

public struct BackupGeneration: Sendable, Equatable, Identifiable {
    public var id: String { generationId }
    public var generationId: String
    public var state: BackupState
    public var startedAtMs: Int64
    public var completedAtMs: Int64?
    public var location: String
    public var objectCount: UInt64?
    public var totalBytes: UInt64?
    public var verification: BackupVerification?

    public init(
        generationId: String, state: BackupState, startedAtMs: Int64,
        completedAtMs: Int64?, location: String, objectCount: UInt64?,
        totalBytes: UInt64?, verification: BackupVerification?
    ) {
        self.generationId = generationId
        self.state = state
        self.startedAtMs = startedAtMs
        self.completedAtMs = completedAtMs
        self.location = location
        self.objectCount = objectCount
        self.totalBytes = totalBytes
        self.verification = verification
    }

    init(_ value: SweetbriarCore.BackupGeneration) {
        self.init(
            generationId: value.generationId, state: BackupState(value.state),
            startedAtMs: value.startedAtMs, completedAtMs: value.completedAtMs,
            location: value.location, objectCount: value.objectCount, totalBytes: value.totalBytes,
            verification: value.verification.map(BackupVerification.init)
        )
    }
}

public enum RestoreArtifactKind: Sendable, Equatable {
    case preserved, staged

    init(_ value: SweetbriarCore.RestoreArtifactKind) {
        switch value {
        case .preserved: self = .preserved
        case .staged: self = .staged
        }
    }
}

/// An unqualified name beside the root, not an ownership/type/readiness claim.
/// Even a staged name may still be in use. Refresh never removes anything.
public struct RestoreArtifact: Sendable, Equatable, Identifiable {
    public var id: String { path }
    public var path: String
    public var kind: RestoreArtifactKind

    public init(path: String, kind: RestoreArtifactKind) {
        self.path = path
        self.kind = kind
    }

    init(_ value: SweetbriarCore.RestoreArtifact) {
        self.init(path: value.path, kind: RestoreArtifactKind(value.kind))
    }
}
