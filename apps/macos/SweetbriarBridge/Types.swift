// The bridge's own public types (spec/08): the app sees these and only
// these — generated `SweetbriarCore` types never cross L1.

import Foundation
import SweetbriarCore

/// Where the core keeps its data. Strings, not URLs, because the FFI
/// boundary is UTF-8 paths (spec/07).
public struct CoreConfig: Sendable, Equatable {
    public var dataRoot: String
    public var cacheRoot: String

    public init(dataRoot: String, cacheRoot: String) {
        self.dataRoot = dataRoot
        self.cacheRoot = cacheRoot
    }
}

/// One registered setting with its effective value (`isDefault` marks
/// a value never explicitly set).
public struct Setting: Sendable, Equatable {
    public var key: String
    public var value: String
    public var isDefault: Bool

    public init(key: String, value: String, isDefault: Bool) {
        self.key = key
        self.value = value
        self.isDefault = isDefault
    }
}

/// What a committed command returns (spec/07 "Commands").
public struct Receipt: Sendable, Equatable {
    public var command: String
    public var resultJson: String
    public var createdAtMs: Int64

    public init(command: String, resultJson: String, createdAtMs: Int64) {
        self.command = command
        self.resultJson = resultJson
        self.createdAtMs = createdAtMs
    }
}

/// Every core failure the shell can see, mirroring `CoreError` over
/// FFI — except `Cancelled`, which callers instead receive as Swift's
/// `CancellationError` (spec/07 "Errors").
public enum CoreFailure: Error, Sendable, Equatable {
    case notFound(what: String)
    case locked
    case unavailable(reason: String)
    case corrupt(detail: String)
    case needsMigration(from: UInt32, to: UInt32)
    case newerStore(appVersion: String)
    case invalidInput(field: String, message: String)
    case conflict(detail: String)
    case resetRequired(generation: UInt64)
    case internalError(detail: String)
}

extension CoreFailure: LocalizedError {
    /// Human-readable text for the window's error state.
    public var errorDescription: String? {
        switch self {
        case .notFound(let what):
            return "Not found: \(what)"
        case .locked:
            return "The store is locked by another process."
        case .unavailable(let reason):
            return "Unavailable: \(reason)"
        case .corrupt(let detail):
            return "The store is corrupt: \(detail)"
        case .needsMigration(let from, let to):
            return "The store needs a migration from schema \(from) to \(to)."
        case .newerStore(let appVersion):
            return "The store was written by a newer Sweetbriar (\(appVersion))."
        case .invalidInput(let field, let message):
            return "Invalid \(field): \(message)"
        case .conflict(let detail):
            return "Conflict: \(detail)"
        case .resetRequired(let generation):
            return "The account needs a reset (generation \(generation))."
        case .internalError(let detail):
            return "Internal error: \(detail)"
        }
    }
}

/// Map the generated error enum onto the bridge's public one.
/// `Cancelled` never surfaces here — it becomes `CancellationError` in
/// `CoreBridge.withCancellation`.
extension CoreFailure {
    init(_ error: SweetbriarCore.CoreError) {
        switch error {
        case .NotFound(let what):
            self = .notFound(what: what)
        case .Locked:
            self = .locked
        case .Unavailable(let reason):
            self = .unavailable(reason: reason)
        case .Corrupt(let detail):
            self = .corrupt(detail: detail)
        case .NeedsMigration(let from, let to):
            self = .needsMigration(from: from, to: to)
        case .NewerStore(let appVersion):
            self = .newerStore(appVersion: appVersion)
        case .InvalidInput(let field, let message):
            self = .invalidInput(field: field, message: message)
        case .Conflict(let detail):
            self = .conflict(detail: detail)
        case .ResetRequired(let generation):
            self = .resetRequired(generation: generation)
        case .Cancelled:
            self = .unavailable(reason: "cancelled")
        case .Internal(let detail):
            self = .internalError(detail: detail)
        }
    }
}
