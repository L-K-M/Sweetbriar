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

/// Required Search defaults; dates with explicit context never consult these.
public struct ParseContext: Sendable, Equatable {
    public var tz: String
    /// UTC milliseconds since the Unix epoch.
    public var clock: Int64

    public init(tz: String, clock: Int64) {
        self.tz = tz
        self.clock = clock
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

/// Where an import lands. The pilot has only the local archive (spec/09).
public enum ImportTarget: Sendable, Equatable {
    case localArchive
}

/// How an mbox escapes `From ` lines (spec/09).
public enum MboxVariant: Sendable, Equatable {
    case mboxo
    case mboxrd
}

/// The variant chosen for one ambiguous source, by inventory ordinal.
public struct VariantChoice: Sendable, Equatable {
    public var ordinal: UInt64
    public var variant: MboxVariant

    public init(ordinal: UInt64, variant: MboxVariant) {
        self.ordinal = ordinal
        self.variant = variant
    }
}

/// What `startImport` answers: its receipt and the job it names.
public struct ImportStarted: Sendable, Equatable {
    public var receipt: Receipt
    public var jobId: String

    public init(receipt: Receipt, jobId: String) {
        self.receipt = receipt
        self.jobId = jobId
    }
}

/// An import job's state (spec/02 `import_jobs`).
public enum ImportJobState: Sendable, Equatable {
    case inventory
    case awaitingConfirmation
    case running
    case completed
    case cancelled
    case failed

    /// Completed, cancelled or failed: nothing more will change.
    public var isFinished: Bool {
        switch self {
        case .completed, .cancelled, .failed:
            return true
        case .inventory, .awaitingConfirmation, .running:
            return false
        }
    }
}

/// What a source holds.
public enum ImportSourceKind: Sendable, Equatable {
    case mbox
    case eml
}

/// Why a chosen or discovered path did not become a source (spec/09).
public enum ImportProblemKind: Sendable, Equatable {
    case inaccessible
    case unsupported
    case nonUtf8
    case failed
}

/// One inventoried source and how far its ingest has got.
public struct ImportSource: Sendable, Equatable {
    public var ordinal: UInt64
    public var path: String
    public var mailbox: String
    public var kind: ImportSourceKind
    public var messages: UInt64
    public var bytes: UInt64
    public var sha256: String
    /// A `>>From ` line exists, so `confirmImport` must choose a variant.
    public var needsVariant: Bool
    public var variant: MboxVariant?
    public var messagesDone: UInt64

    public init(
        ordinal: UInt64,
        path: String,
        mailbox: String,
        kind: ImportSourceKind,
        messages: UInt64,
        bytes: UInt64,
        sha256: String,
        needsVariant: Bool,
        variant: MboxVariant?,
        messagesDone: UInt64
    ) {
        self.ordinal = ordinal
        self.path = path
        self.mailbox = mailbox
        self.kind = kind
        self.messages = messages
        self.bytes = bytes
        self.sha256 = sha256
        self.needsVariant = needsVariant
        self.variant = variant
        self.messagesDone = messagesDone
    }
}

/// A path that did not become a source, and why.
public struct ImportProblem: Sendable, Equatable {
    public var path: String
    public var kind: ImportProblemKind
    public var detail: String

    public init(path: String, kind: ImportProblemKind, detail: String) {
        self.path = path
        self.kind = kind
        self.detail = detail
    }
}

/// Running totals (spec/09 "Report").
public struct ImportTotals: Sendable, Equatable {
    public var processed: UInt64
    public var imported: UInt64
    public var duplicates: UInt64
    public var rejected: UInt64
    public var bytesStored: UInt64
    public var readFromHeaders: UInt64
    public var readDefault: UInt64
    public var flagsIncomplete: UInt64

    public init(
        processed: UInt64,
        imported: UInt64,
        duplicates: UInt64,
        rejected: UInt64,
        bytesStored: UInt64,
        readFromHeaders: UInt64,
        readDefault: UInt64,
        flagsIncomplete: UInt64
    ) {
        self.processed = processed
        self.imported = imported
        self.duplicates = duplicates
        self.rejected = rejected
        self.bytesStored = bytesStored
        self.readFromHeaders = readFromHeaders
        self.readDefault = readDefault
        self.flagsIncomplete = flagsIncomplete
    }
}

/// One import job as `importJob` reports it (spec/07).
public struct ImportJob: Sendable, Equatable {
    public var id: String
    public var state: ImportJobState
    /// The inventory in ordinal order; empty until it finishes.
    public var sources: [ImportSource]
    /// The first problems; `problemCount` counts them all.
    public var problems: [ImportProblem]
    public var problemCount: UInt64
    public var totals: ImportTotals
    public var failure: String?
    /// The report's path, once it is written (spec/09).
    public var reportPath: String?

    public init(
        id: String,
        state: ImportJobState,
        sources: [ImportSource],
        problems: [ImportProblem],
        problemCount: UInt64,
        totals: ImportTotals,
        failure: String?,
        reportPath: String?
    ) {
        self.id = id
        self.state = state
        self.sources = sources
        self.problems = problems
        self.problemCount = problemCount
        self.totals = totals
        self.failure = failure
        self.reportPath = reportPath
    }
}

/// A finished job's published report (spec/07 `import_report`).
public struct ImportReport: Sendable, Equatable {
    public var path: String
    /// The version-1 JSON object (spec/09 "Report").
    public var json: String

    public init(path: String, json: String) {
        self.path = path
        self.json = json
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

/// Which window the shell is listing (spec/07 "Store windows").
public enum WindowSpec: Sendable, Equatable, Hashable {
    /// The synthetic source filled by `debugFillSynthetic`.
    case debugSynthetic
    /// One mailbox's occurrences.
    case mailbox(Int64)
    /// Every occurrence in mailboxes of one role.
    case unified(MailboxRole)
    /// Flagged logical messages, once each.
    case flagged
    /// One thread's logical messages, once each.
    case thread(Int64)
}

/// What a window's subscription reports (spec/07 "Subscriptions").
public enum WindowEvent: Sendable, Equatable {
    /// The window's rows changed; re-read at this generation.
    case invalidated(generation: UInt64)
    /// Fallen too far behind: rebuild the id index, then the pages.
    case resetRequired
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

/// Stable query parse codes. Spans remain UTF-8 bytes until the editor maps them.
public enum SearchParseCode: String, Sendable, Equatable {
    case unexpectedToken = "unexpected_token"
    case missingValue = "missing_value"
    case unclosedQuote = "unclosed_quote"
    case unclosedGroup = "unclosed_group"
    case unknownKey = "unknown_key"
    case invalidOperator = "invalid_operator"
    case invalidValue = "invalid_value"
    case limit

    init(_ code: SweetbriarCore.SearchParseCode) {
        switch code {
        case .unexpectedToken: self = .unexpectedToken
        case .missingValue: self = .missingValue
        case .unclosedQuote: self = .unclosedQuote
        case .unclosedGroup: self = .unclosedGroup
        case .unknownKey: self = .unknownKey
        case .invalidOperator: self = .invalidOperator
        case .invalidValue: self = .invalidValue
        case .limit: self = .limit
        }
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
    case searchParse(start: UInt32, end: UInt32, code: SearchParseCode)
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
        case .searchParse(let start, let end, let code):
            return "Search \(code.rawValue) at bytes [\(start),\(end))."
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
        case .SearchParse(let start, let end, let code):
            self = .searchParse(start: start, end: end, code: SearchParseCode(code))
        }
    }
}

/// Recorded availability; this does not promise filesystem integrity.
public enum BodyAvailability: Sendable, Equatable {
    case notAcquired, acquired, acquisitionMissing, acquisitionCorrupt
}

public struct ReaderAddress: Sendable, Equatable {
    public var name: String?
    public var address: String?

    public init(name: String?, address: String?) {
        self.name = name
        self.address = address
    }
}

public struct ReaderHeaders: Sendable, Equatable {
    public var from: [ReaderAddress]
    public var to: [ReaderAddress]
    public var cc: [ReaderAddress]
    public var date: Int64?
    public var subject: String?
    public var messageId: String?

    public init(from: [ReaderAddress], to: [ReaderAddress], cc: [ReaderAddress], date: Int64?, subject: String?, messageId: String?) {
        self.from = from
        self.to = to
        self.cc = cc
        self.date = date
        self.subject = subject
        self.messageId = messageId
    }
}

public struct ReaderOccurrence: Sendable, Equatable {
    public var occurrenceId: Int64
    public var mailboxId: Int64
    public var mailboxName: String

    public init(occurrenceId: Int64, mailboxId: Int64, mailboxName: String) {
        self.occurrenceId = occurrenceId
        self.mailboxId = mailboxId
        self.mailboxName = mailboxName
    }
}

public struct ReaderTag: Sendable, Equatable {
    public var tagId: String
    public var name: String
    public var colorSrgb: String?

    public init(tagId: String, name: String, colorSrgb: String?) {
        self.tagId = tagId
        self.name = name
        self.colorSrgb = colorSrgb
    }
}

public struct ReaderMessage: Sendable, Equatable {
    public var messageId: Int64
    public var headers: ReaderHeaders
    public var accountId: String
    public var accountDisplayName: String
    public var occurrences: [ReaderOccurrence]
    public var threadId: Int64?
    public var tags: [ReaderTag]
    public var note: String?
    public var bodyAvailability: BodyAvailability

    public init(messageId: Int64, headers: ReaderHeaders, accountId: String, accountDisplayName: String, occurrences: [ReaderOccurrence], threadId: Int64?, tags: [ReaderTag], note: String?, bodyAvailability: BodyAvailability) {
        self.messageId = messageId
        self.headers = headers
        self.accountId = accountId
        self.accountDisplayName = accountDisplayName
        self.occurrences = occurrences
        self.threadId = threadId
        self.tags = tags
        self.note = note
        self.bodyAvailability = bodyAvailability
    }
}

/// The rendition requested, or the actual contents of a returned lease.
public enum BodyKind: Sendable, Equatable {
    case sanitizedHtml, plainText, source
}

public enum RenderWarningCode: Sendable, Equatable {
    case partialParse, charsetReplaced, unsupportedContent, missingCid
    case ambiguousCid, unsafeUrl, renditionLimit, countSaturated
}

public struct RenderWarning: Sendable, Equatable {
    public var code: RenderWarningCode
    public var partId: String?

    public init(code: RenderWarningCode, partId: String?) {
        self.code = code
        self.partId = partId
    }
}

/// Cache files remain valid until release or core shutdown.
public struct BodyLease: Sendable, Equatable {
    public var leaseId: String
    public var kind: BodyKind
    public var filePath: String
    public var byteLength: UInt64
    public var remoteContentBlocked: UInt32
    public var warnings: [RenderWarning]
    public var manifestPath: String?

    public init(leaseId: String, kind: BodyKind, filePath: String, byteLength: UInt64,
                remoteContentBlocked: UInt32, warnings: [RenderWarning], manifestPath: String?) {
        self.leaseId = leaseId
        self.kind = kind
        self.filePath = filePath
        self.byteLength = byteLength
        self.remoteContentBlocked = remoteContentBlocked
        self.warnings = warnings
        self.manifestPath = manifestPath
    }
}

/// A decoded attachment, valid until release or core shutdown.
public struct AttachmentLease: Sendable, Equatable {
    public var leaseId: String
    public var filePath: String
    public var byteLength: UInt64
    public var partId: String
    public var displayName: String
    public var mediaType: String

    public init(leaseId: String, filePath: String, byteLength: UInt64,
                partId: String, displayName: String, mediaType: String) {
        self.leaseId = leaseId
        self.filePath = filePath
        self.byteLength = byteLength
        self.partId = partId
        self.displayName = displayName
        self.mediaType = mediaType
    }
}
