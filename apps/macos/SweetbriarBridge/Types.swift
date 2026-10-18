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

/// How an account reaches its mail.
public enum AccountKind: Sendable, Equatable {
    case local
    case imap
}

/// Whether an account syncs.
public enum AccountState: Sendable, Equatable {
    case active
    case paused
    case needsAttention
    case disconnected
}

/// One account (spec/07 `accounts()`).
public struct Account: Sendable, Equatable {
    public var accountId: String
    public var kind: AccountKind
    public var displayName: String
    public var state: AccountState
    public var stateReason: String?
    public var sortOrder: Int64

    public init(
        accountId: String,
        kind: AccountKind,
        displayName: String,
        state: AccountState,
        stateReason: String?,
        sortOrder: Int64
    ) {
        self.accountId = accountId
        self.kind = kind
        self.displayName = displayName
        self.state = state
        self.stateReason = stateReason
        self.sortOrder = sortOrder
    }
}

/// A mailbox's special-use role (spec/02 `mailboxes.role`).
public enum MailboxRole: Sendable, Equatable, Hashable {
    case inbox
    case sent
    case drafts
    case trash
    case junk
    case archive
    case all
    /// Bridge's Starred (spec/04).
    case flagged
}

/// What a mailbox is.
public enum MailboxKind: Sendable, Equatable {
    case folder
    case label
    case virtual
}

/// One mailbox, with its present occurrences and the unread ones
/// (spec/07 `mailboxes(account_id)`).
public struct Mailbox: Sendable, Equatable {
    public var mailboxId: Int64
    public var accountId: String
    public var name: String
    public var delimiter: String?
    public var role: MailboxRole?
    public var kind: MailboxKind
    public var selectable: Bool
    public var total: UInt64
    public var unread: UInt64

    public init(
        mailboxId: Int64,
        accountId: String,
        name: String,
        delimiter: String?,
        role: MailboxRole?,
        kind: MailboxKind,
        selectable: Bool,
        total: UInt64,
        unread: UInt64
    ) {
        self.mailboxId = mailboxId
        self.accountId = accountId
        self.name = name
        self.delimiter = delimiter
        self.role = role
        self.kind = kind
        self.selectable = selectable
        self.total = total
        self.unread = unread
    }
}

/// An account's mailboxes as of one list generation.
public struct Mailboxes: Sendable, Equatable {
    public var generation: UInt64
    public var mailboxes: [Mailbox]

    public init(generation: UInt64, mailboxes: [Mailbox]) {
        self.generation = generation
        self.mailboxes = mailboxes
    }
}

/// Which window the shell is listing (spec/07 "Queries"). For the
/// pilot only `debugSynthetic` — the in-memory debug source — exists.
public enum WindowSpec: Sendable, Equatable {
    /// The synthetic source filled by `debugFillSynthetic`.
    case debugSynthetic
}

/// Where a window page read starts (spec/07 "Queries"). Cursors are
/// opaque strings minted by the core; the shell carries them
/// unchanged.
public enum Anchor: Sendable, Equatable {
    /// The newest rows: the first page.
    case top
    /// At most `limit` rows strictly after the cursor's row.
    case after(String)
    /// At most `limit` rows strictly before the cursor's row.
    case before(String)
    /// The page centred on this occurrence id.
    case around(Int64)
}

/// A window's row count as the shell should treat it (spec/07).
public enum ListCount: Sendable, Equatable {
    /// The exact number of rows.
    case exact(n: UInt64)
    /// A lower bound on the number of rows.
    case atLeast(n: UInt64)
    /// Not known.
    case unknown
}

/// One list row (spec/07): display fields only — no MIME tree, no
/// body. The shell resolves the account color from `accountId`.
public struct ListRow: Sendable, Equatable {
    public var occurrenceId: Int64
    public var messageId: Int64
    public var accountId: String
    public var mailboxId: Int64
    public var sortDate: Int64
    public var sender: String
    public var subject: String
    public var snippet: String
    public var unread: Bool
    public var flagged: Bool
    public var hasAttachments: Bool
    public var tags: [String]

    public init(
        occurrenceId: Int64,
        messageId: Int64,
        accountId: String,
        mailboxId: Int64,
        sortDate: Int64,
        sender: String,
        subject: String,
        snippet: String,
        unread: Bool,
        flagged: Bool,
        hasAttachments: Bool,
        tags: [String]
    ) {
        self.occurrenceId = occurrenceId
        self.messageId = messageId
        self.accountId = accountId
        self.mailboxId = mailboxId
        self.sortDate = sortDate
        self.sender = sender
        self.subject = subject
        self.snippet = snippet
        self.unread = unread
        self.flagged = flagged
        self.hasAttachments = hasAttachments
        self.tags = tags
    }
}

/// One page of a window (spec/07 "Queries"): the rows in window order,
/// the cursors continuing the page — `nil` when it is empty — and
/// whether rows lie past each end.
public struct ListWindow: Sendable, Equatable {
    public var windowId: UInt64
    public var generation: UInt64
    public var rows: [ListRow]
    public var before: String?
    public var after: String?
    public var hasMoreAfter: Bool
    public var hasMoreBefore: Bool
    public var total: ListCount

    public init(
        windowId: UInt64,
        generation: UInt64,
        rows: [ListRow],
        before: String?,
        after: String?,
        hasMoreAfter: Bool,
        hasMoreBefore: Bool,
        total: ListCount
    ) {
        self.windowId = windowId
        self.generation = generation
        self.rows = rows
        self.before = before
        self.after = after
        self.hasMoreAfter = hasMoreAfter
        self.hasMoreBefore = hasMoreBefore
        self.total = total
    }
}

/// A window's id index (spec/07 "The id index"): every row's
/// `occurrenceId` in window order at the reported generation.
public struct WindowIDs: Sendable, Equatable {
    public var generation: UInt64
    public var ids: [Int64]

    public init(generation: UInt64, ids: [Int64]) {
        self.generation = generation
        self.ids = ids
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
