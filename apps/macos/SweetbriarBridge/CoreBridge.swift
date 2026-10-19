// `CoreBridge`: the L1 boundary (spec/08). It owns the generated
// `Core`, gives every call a fresh `CancelToken` wired to the calling
// task's cancellation, and publishes settings snapshots through the
// owner's `FrameBatcher`. App code never imports `SweetbriarCore`.

import Foundation
import SweetbriarCore

/// A live core. `open` it, call it, `shutdown()` before exit.
///
/// Every call runs inside `withTaskCancellationHandler`: cancelling
/// the caller's task cancels the call's `CancelToken`, and a
/// `Cancelled` from the core comes back to the caller as Swift's
/// `CancellationError` (spec/07). `shutdown()` stays uncancellable.
public final class CoreBridge: Sendable {
    private let core: SweetbriarCore.Core
    private let snapshots: FrameBatcher<[Setting]>

    private init(core: SweetbriarCore.Core, snapshots: FrameBatcher<[Setting]>) {
        self.core = core
        self.snapshots = snapshots
    }

    /// Open the core under explicit roots.
    public static func open(
        config: CoreConfig,
        snapshots: FrameBatcher<[Setting]>
    ) async throws -> CoreBridge {
        let token = SweetbriarCore.CancelToken()
        let core = try await withCancellation(token: token) {
            try await SweetbriarCore.openCore(
                token: token,
                config: SweetbriarCore.CoreConfig(
                    dataRoot: config.dataRoot,
                    cacheRoot: config.cacheRoot
                )
            )
        }
        return CoreBridge(core: core, snapshots: snapshots)
    }

    /// Open the core under the platform's default roots.
    public static func open(
        snapshots: FrameBatcher<[Setting]>
    ) async throws -> CoreBridge {
        let config: SweetbriarCore.CoreConfig
        do {
            config = try SweetbriarCore.platformDefaultConfig()
        } catch let error as SweetbriarCore.CoreError {
            throw CoreFailure(error)
        }
        return try await open(
            config: CoreConfig(
                dataRoot: config.dataRoot,
                cacheRoot: config.cacheRoot
            ),
            snapshots: snapshots
        )
    }

    /// Read the message headers, account and read-only metadata.
    public func getMessage(messageId: Int64) async throws -> ReaderMessage {
        let token = SweetbriarCore.CancelToken()
        let value = try await Self.withCancellation(token: token) {
            try await self.core.getMessage(token: token, messageId: messageId)
        }
        return ReaderMessage(value)
    }

    /// Open a body file. Release the lease when the reader discards it.
    public func openBody(messageID: Int64, preferred: BodyKind) async throws -> BodyLease {
        let token = SweetbriarCore.CancelToken()
        let value = try await Self.withCancellation(token: token) {
            try await self.core.openBody(token: token, messageId: messageID, preferred: preferred.generated)
        }
        return BodyLease(value)
    }

    /// Open a decoded attachment. Release the lease when finished with the file.
    public func openAttachment(messageID: Int64, partID: String) async throws -> AttachmentLease {
        let token = SweetbriarCore.CancelToken()
        let value = try await Self.withCancellation(token: token) {
            try await self.core.openAttachment(token: token, messageId: messageID, partId: partID)
        }
        return AttachmentLease(value)
    }

    /// Revoke immediately; file cleanup runs on the core's worker.
    public func releaseLease(_ leaseID: String) {
        core.releaseLease(leaseId: leaseID)
    }

    /// Resolve a list selection to its logical message.
    public func messageForOccurrence(occurrenceId: Int64) async throws -> Int64 {
        let token = SweetbriarCore.CancelToken()
        let value = try await Self.withCancellation(token: token) {
            try await self.core.messageForOccurrence(token: token, occurrenceId: occurrenceId)
        }
        return value
    }

    /// Every registered setting with its effective value. The snapshot
    /// also goes to the batcher for `@MainActor` observers.
    public func settings() async throws -> [Setting] {
        let token = SweetbriarCore.CancelToken()
        let read = try await Self.withCancellation(token: token) {
            try await self.core.settings(token: token)
        }
        let snapshot = read.map(Setting.init)
        snapshots.post(snapshot)
        return snapshot
    }

    /// Write one setting under a fresh `UUID` idempotency key.
    ///
    /// Once the core has begun the command it runs to completion
    /// (spec/07): the receipt comes back even if the token is
    /// cancelled meanwhile. The changed snapshot is then published on
    /// a token of its own, so a caller cancelled after the commit
    /// can't leave observers showing the old value; a failed re-read
    /// leaves them until the next snapshot.
    @discardableResult
    public func setSetting(key: String, value: String) async throws -> Receipt {
        let token = SweetbriarCore.CancelToken()
        let receipt = try await Self.withCancellation(token: token) {
            try await self.core.setSetting(
                token: token,
                idempotencyKey: UUID().uuidString,
                key: key,
                value: value
            )
        }
        let fresh = try? await core.settings(token: SweetbriarCore.CancelToken())
        if let fresh {
            snapshots.post(fresh.map(Setting.init))
        }
        return Receipt(receipt)
    }

    /// `start_import(paths, target)` (spec/07, 09) under a fresh `UUID`
    /// idempotency key: record the job and inventory it in the
    /// background. Like every command, once begun it runs to
    /// completion.
    public func startImport(
        paths: [String],
        target: ImportTarget = .localArchive
    ) async throws -> ImportStarted {
        let token = SweetbriarCore.CancelToken()
        let started = try await Self.withCancellation(token: token) {
            try await self.core.startImport(
                token: token,
                idempotencyKey: UUID().uuidString,
                paths: paths,
                target: target.generated
            )
        }
        return ImportStarted(receipt: Receipt(started.receipt), jobId: started.jobId)
    }

    /// `confirm_import(job_id, variants)` (spec/07, 09): a variant for
    /// exactly the sources that need one; then ingest. A missing,
    /// extra or repeated choice is `invalidInput`.
    @discardableResult
    public func confirmImport(
        jobId: String,
        variants: [VariantChoice]
    ) async throws -> Receipt {
        let token = SweetbriarCore.CancelToken()
        let receipt = try await Self.withCancellation(token: token) {
            try await self.core.confirmImport(
                token: token,
                idempotencyKey: UUID().uuidString,
                jobId: jobId,
                variants: variants.map(\.generated)
            )
        }
        return Receipt(receipt)
    }

    /// `cancel_job(id)` (spec/07, 09): the job is cancelled once this
    /// returns; committed batches stay imported. A finished job is
    /// `conflict`.
    @discardableResult
    public func cancelJob(jobId: String) async throws -> Receipt {
        let token = SweetbriarCore.CancelToken()
        let receipt = try await Self.withCancellation(token: token) {
            try await self.core.cancelJob(
                token: token,
                idempotencyKey: UUID().uuidString,
                jobId: jobId
            )
        }
        return Receipt(receipt)
    }

    /// `import_job(job_id)` (spec/07): state, inventory, problems and
    /// counts. An unknown job is `notFound`.
    public func importJob(id: String) async throws -> ImportJob {
        let token = SweetbriarCore.CancelToken()
        let job = try await Self.withCancellation(token: token) {
            try await self.core.importJob(token: token, jobId: id)
        }
        return ImportJob(job)
    }

    /// `import_report(job_id)` (spec/07, 09): a finished job's report,
    /// or `nil` while it is still being written. An unfinished job is
    /// `conflict`.
    public func importReport(jobId: String) async throws -> ImportReport? {
        let token = SweetbriarCore.CancelToken()
        let report = try await Self.withCancellation(token: token) {
            try await self.core.importReport(token: token, jobId: jobId)
        }
        return report.map { ImportReport(path: $0.path, json: $0.json) }
    }

    /// `accounts()` (spec/07): every account, by sort order then id.
    public func accounts() async throws -> [Account] {
        let token = SweetbriarCore.CancelToken()
        let accounts = try await Self.withCancellation(token: token) {
            try await self.core.accounts(token: token)
        }
        return accounts.map(Account.init)
    }

    /// `mailboxes(account_id)` (spec/07): the account's mailboxes with
    /// their counts, as of one list generation. An unknown account is
    /// `notFound`.
    public func mailboxes(accountId: String) async throws -> Mailboxes {
        let token = SweetbriarCore.CancelToken()
        let list = try await Self.withCancellation(token: token) {
            try await self.core.mailboxes(token: token, accountId: accountId)
        }
        return Mailboxes(
            generation: list.generation,
            mailboxes: list.mailboxes.map(Mailbox.init)
        )
    }

    /// `open_window` (spec/07): register a window over `spec` and
    /// return its first page at the source's current generation.
    public func openWindow(
        spec: WindowSpec,
        anchor: Anchor,
        limit: UInt32
    ) async throws -> ListWindow {
        let token = SweetbriarCore.CancelToken()
        let window = try await Self.withCancellation(token: token) {
            try await self.core.openWindow(
                token: token,
                spec: spec.generated,
                anchor: anchor.generated,
                limit: limit
            )
        }
        return ListWindow(window)
    }

    /// `read_window` (spec/07): the page `anchor` picks out of the
    /// window at the generation the caller holds. A stale generation
    /// or cursor is `resetRequired`; an unknown or closed window is
    /// `notFound`.
    public func readWindow(
        id: UInt64,
        generation: UInt64,
        anchor: Anchor,
        limit: UInt32
    ) async throws -> ListWindow {
        let token = SweetbriarCore.CancelToken()
        // Signpost "ffi" (plan M0.10): the model's read crossing the
        // bridge into the core.
        let span = ListPrototypeSignposts.begin("ffi")
        defer { ListPrototypeSignposts.end("ffi", span) }
        let window = try await Self.withCancellation(token: token) {
            try await self.core.readWindow(
                token: token,
                windowId: id,
                generation: generation,
                anchor: anchor.generated,
                limit: limit
            )
        }
        return ListWindow(window)
    }

    /// `window_ids` (spec/07 "The id index"): every row's occurrence
    /// id in window order, with the source's current generation.
    public func windowIDs(id: UInt64) async throws -> WindowIDs {
        let token = SweetbriarCore.CancelToken()
        let index = try await Self.withCancellation(token: token) {
            try await self.core.windowIds(token: token, windowId: id)
        }
        return WindowIDs(generation: index.generation, ids: index.ids)
    }

    /// `subscribe` (spec/07 "Subscriptions"): the window's changes from
    /// what it last delivered, pulled with `WindowSubscription.next()`.
    public func subscribe(windowId: UInt64) async throws -> WindowSubscription {
        let token = SweetbriarCore.CancelToken()
        let subscription = try await Self.withCancellation(token: token) {
            try await self.core.subscribe(token: token, windowId: windowId)
        }
        return WindowSubscription(subscription)
    }

    /// `close_window` (spec/07): release the handle. No token — like
    /// `shutdown`, the call can't be cancelled; closing twice is
    /// harmless.
    public func closeWindow(id: UInt64) async {
        await core.closeWindow(windowId: id)
    }

    /// `debug_fill_synthetic` (spec/07 "Debug source"): replace the
    /// synthetic source with `count` rows and return the new
    /// generation. The token counts only until the replacement.
    public func debugFillSynthetic(count: UInt32) async throws -> UInt64 {
        let token = SweetbriarCore.CancelToken()
        return try await Self.withCancellation(token: token) {
            try await self.core.debugFillSynthetic(token: token, count: count)
        }
    }

    /// Close the core. Uncancellable by spec/07: it waits for calls in
    /// flight, so the caller just awaits it.
    public func shutdown() async {
        await core.shutdown()
    }

    /// Run `work` with `token` bound to the surrounding task's
    /// cancellation, and translate core errors into the bridge's
    /// public surface: `Cancelled` becomes `CancellationError`, the
    /// rest `CoreFailure`. Internal, not private: tests drive it with
    /// their own `CallToken` to hold a call in flight.
    static func withCancellation<T>(
        token: some CallToken,
        _ work: () async throws -> T
    ) async throws -> T {
        try await withTaskCancellationHandler {
            do {
                return try await work()
            } catch let error as SweetbriarCore.CoreError {
                if case .Cancelled = error {
                    throw CancellationError()
                }
                throw CoreFailure(error)
            }
        } onCancel: {
            token.cancel()
        }
    }
}

/// One window's stream of events (spec/07 "Subscriptions"). Pull with
/// `next()`, one call at a time; cancelling the calling task cancels
/// the call, and a pending call ends with `unavailable` once the core
/// shuts down.
public final class WindowSubscription: Sendable {
    private let subscription: SweetbriarCore.Subscription

    fileprivate init(_ subscription: SweetbriarCore.Subscription) {
        self.subscription = subscription
    }

    /// The next event: at once when the window's source already moved
    /// past the last one, else when it does. A closed window is
    /// `notFound`; a second concurrent call is `invalidInput`.
    public func next() async throws -> WindowEvent {
        let token = SweetbriarCore.CancelToken()
        let event = try await CoreBridge.withCancellation(token: token) {
            try await self.subscription.next(token: token)
        }
        switch event {
        case .invalidated(let generation):
            return .invalidated(generation: generation)
        case .resetRequired:
            return .resetRequired
        }
    }
}

/// What `withCancellation` cancels when the calling task is
/// cancelled: the core's `CancelToken`, or a test's stand-in.
protocol CallToken: Sendable {
    func cancel()
}

extension SweetbriarCore.CancelToken: CallToken {}

extension CoreBridge: ListWindowSource {
    public func windowEvents(id: UInt64) async throws -> any WindowEventSource {
        try await subscribe(windowId: id)
    }
}

extension WindowSubscription: WindowEventSource {}

extension Setting {
    /// Map the generated record field by field.
    fileprivate init(_ generated: SweetbriarCore.Setting) {
        self.init(
            key: generated.key,
            value: generated.value,
            isDefault: generated.isDefault
        )
    }
}

extension Receipt {
    /// Map the generated record field by field.
    fileprivate init(_ generated: SweetbriarCore.Receipt) {
        self.init(
            command: generated.command,
            resultJson: generated.resultJson,
            createdAtMs: generated.createdAtMs
        )
    }
}

extension ImportTarget {
    /// Map the bridge enum onto the generated one.
    fileprivate var generated: SweetbriarCore.ImportTarget {
        switch self {
        case .localArchive:
            return .localArchive
        }
    }
}

extension VariantChoice {
    /// Map the bridge record onto the generated one.
    fileprivate var generated: SweetbriarCore.VariantChoice {
        let variant: SweetbriarCore.MboxVariant
        switch self.variant {
        case .mboxo:
            variant = .mboxo
        case .mboxrd:
            variant = .mboxrd
        }
        return SweetbriarCore.VariantChoice(ordinal: ordinal, variant: variant)
    }
}

extension MboxVariant {
    /// Map the generated enum case by case.
    fileprivate init(_ generated: SweetbriarCore.MboxVariant) {
        switch generated {
        case .mboxo:
            self = .mboxo
        case .mboxrd:
            self = .mboxrd
        }
    }
}

extension ImportJobState {
    /// Map the generated enum case by case.
    fileprivate init(_ generated: SweetbriarCore.ImportJobState) {
        switch generated {
        case .inventory:
            self = .inventory
        case .awaitingConfirmation:
            self = .awaitingConfirmation
        case .running:
            self = .running
        case .completed:
            self = .completed
        case .cancelled:
            self = .cancelled
        case .failed:
            self = .failed
        }
    }
}

extension ImportSource {
    /// Map the generated record field by field.
    fileprivate init(_ generated: SweetbriarCore.ImportSource) {
        let kind: ImportSourceKind
        switch generated.kind {
        case .mbox:
            kind = .mbox
        case .eml:
            kind = .eml
        }
        self.init(
            ordinal: generated.ordinal,
            path: generated.path,
            mailbox: generated.mailbox,
            kind: kind,
            messages: generated.messages,
            bytes: generated.bytes,
            sha256: generated.sha256,
            needsVariant: generated.needsVariant,
            variant: generated.variant.map(MboxVariant.init),
            messagesDone: generated.messagesDone
        )
    }
}

extension ImportProblem {
    /// Map the generated record field by field.
    fileprivate init(_ generated: SweetbriarCore.ImportProblem) {
        let kind: ImportProblemKind
        switch generated.kind {
        case .inaccessible:
            kind = .inaccessible
        case .unsupported:
            kind = .unsupported
        case .nonUtf8:
            kind = .nonUtf8
        case .failed:
            kind = .failed
        }
        self.init(path: generated.path, kind: kind, detail: generated.detail)
    }
}

extension ImportJob {
    /// Map the generated record field by field.
    fileprivate init(_ generated: SweetbriarCore.ImportJob) {
        let totals = generated.totals
        self.init(
            id: generated.id,
            state: ImportJobState(generated.state),
            sources: generated.sources.map(ImportSource.init),
            problems: generated.problems.map(ImportProblem.init),
            problemCount: generated.problemCount,
            totals: ImportTotals(
                processed: totals.processed,
                imported: totals.imported,
                duplicates: totals.duplicates,
                rejected: totals.rejected,
                bytesStored: totals.bytesStored,
                readFromHeaders: totals.readFromHeaders,
                readDefault: totals.readDefault,
                flagsIncomplete: totals.flagsIncomplete
            ),
            failure: generated.failure,
            reportPath: generated.reportPath
        )
    }
}

extension Account {
    /// Map the generated record field by field.
    fileprivate init(_ generated: SweetbriarCore.Account) {
        let kind: AccountKind
        switch generated.kind {
        case .local:
            kind = .local
        case .imap:
            kind = .imap
        }
        let state: AccountState
        switch generated.state {
        case .active:
            state = .active
        case .paused:
            state = .paused
        case .needsAttention:
            state = .needsAttention
        case .disconnected:
            state = .disconnected
        }
        self.init(
            accountId: generated.accountId,
            kind: kind,
            displayName: generated.displayName,
            state: state,
            stateReason: generated.stateReason,
            sortOrder: generated.sortOrder
        )
    }
}

extension MailboxRole {
    /// Map the generated enum case by case.
    fileprivate init(_ generated: SweetbriarCore.MailboxRole) {
        switch generated {
        case .inbox:
            self = .inbox
        case .sent:
            self = .sent
        case .drafts:
            self = .drafts
        case .trash:
            self = .trash
        case .junk:
            self = .junk
        case .archive:
            self = .archive
        case .all:
            self = .all
        case .flagged:
            self = .flagged
        }
    }

    /// Map the bridge enum onto the generated one.
    fileprivate var generated: SweetbriarCore.MailboxRole {
        switch self {
        case .inbox:
            return .inbox
        case .sent:
            return .sent
        case .drafts:
            return .drafts
        case .trash:
            return .trash
        case .junk:
            return .junk
        case .archive:
            return .archive
        case .all:
            return .all
        case .flagged:
            return .flagged
        }
    }
}

extension Mailbox {
    /// Map the generated record field by field.
    fileprivate init(_ generated: SweetbriarCore.Mailbox) {
        let kind: MailboxKind
        switch generated.kind {
        case .folder:
            kind = .folder
        case .label:
            kind = .label
        case .virtual:
            kind = .virtual
        }
        self.init(
            mailboxId: generated.mailboxId,
            accountId: generated.accountId,
            name: generated.name,
            delimiter: generated.delimiter,
            role: generated.role.map(MailboxRole.init),
            kind: kind,
            selectable: generated.selectable,
            total: generated.total,
            unread: generated.unread
        )
    }
}

extension WindowSpec {
    /// Map the bridge enum onto the generated one.
    fileprivate var generated: SweetbriarCore.WindowSpec {
        switch self {
        case .debugSynthetic:
            return .debugSynthetic
        case .mailbox(let mailboxId):
            return .mailbox(mailboxId: mailboxId)
        case .unified(let role):
            return .unified(role: role.generated)
        case .flagged:
            return .flagged
        case .thread(let threadId):
            return .thread(threadId: threadId)
        }
    }
}

extension Anchor {
    /// Map the bridge enum onto the generated one.
    fileprivate var generated: SweetbriarCore.Anchor {
        switch self {
        case .top:
            return .top
        case .after(let cursor):
            return .after(cursor: cursor)
        case .before(let cursor):
            return .before(cursor: cursor)
        case .around(let occurrenceId):
            return .around(occurrenceId: occurrenceId)
        }
    }
}

extension ListCount {
    /// Map the generated enum case by case.
    fileprivate init(_ generated: SweetbriarCore.Count) {
        switch generated {
        case .exact(let n):
            self = .exact(n: n)
        case .atLeast(let n):
            self = .atLeast(n: n)
        case .unknown:
            self = .unknown
        }
    }
}

extension ListRow {
    /// Map the generated record field by field.
    fileprivate init(_ generated: SweetbriarCore.ListRow) {
        self.init(
            occurrenceId: generated.occurrenceId,
            messageId: generated.messageId,
            accountId: generated.accountId,
            mailboxId: generated.mailboxId,
            sortDate: generated.sortDate,
            sender: generated.sender,
            subject: generated.subject,
            snippet: generated.snippet,
            unread: generated.unread,
            flagged: generated.flagged,
            hasAttachments: generated.hasAttachments,
            tags: generated.tags
        )
    }
}

extension ListWindow {
    /// Map the generated record field by field.
    fileprivate init(_ generated: SweetbriarCore.Window) {
        self.init(
            windowId: generated.windowId,
            generation: generated.generation,
            rows: generated.rows.map { ListRow($0) },
            before: generated.before,
            after: generated.after,
            hasMoreAfter: generated.hasMoreAfter,
            hasMoreBefore: generated.hasMoreBefore,
            total: ListCount(generated.total)
        )
    }
}

extension BodyAvailability {
    fileprivate init(_ generated: SweetbriarCore.BodyAvailability) {
        switch generated {
        case .notAcquired: self = .notAcquired
        case .acquired: self = .acquired
        case .acquisitionMissing: self = .acquisitionMissing
        case .acquisitionCorrupt: self = .acquisitionCorrupt
        }
    }
}

extension ReaderAddress {
    fileprivate init(_ generated: SweetbriarCore.ReaderAddress) {
        self.init(
            name: generated.name,
            address: generated.address
        )
    }
}

extension ReaderHeaders {
    fileprivate init(_ generated: SweetbriarCore.ReaderHeaders) {
        self.init(
            from: generated.from.map { ReaderAddress($0) },
            to: generated.to.map { ReaderAddress($0) },
            cc: generated.cc.map { ReaderAddress($0) },
            date: generated.date,
            subject: generated.subject,
            messageId: generated.messageId
        )
    }
}

extension ReaderOccurrence {
    fileprivate init(_ generated: SweetbriarCore.ReaderOccurrence) {
        self.init(
            occurrenceId: generated.occurrenceId,
            mailboxId: generated.mailboxId,
            mailboxName: generated.mailboxName
        )
    }
}

extension ReaderTag {
    fileprivate init(_ generated: SweetbriarCore.ReaderTag) {
        self.init(
            tagId: generated.tagId,
            name: generated.name,
            colorSrgb: generated.colorSrgb
        )
    }
}

extension ReaderMessage {
    fileprivate init(_ generated: SweetbriarCore.ReaderMessage) {
        self.init(
            messageId: generated.messageId,
            headers: ReaderHeaders(generated.headers),
            accountId: generated.accountId,
            accountDisplayName: generated.accountDisplayName,
            occurrences: generated.occurrences.map { ReaderOccurrence($0) },
            threadId: generated.threadId,
            tags: generated.tags.map { ReaderTag($0) },
            note: generated.note,
            bodyAvailability: BodyAvailability(generated.bodyAvailability)
        )
    }
}

extension BodyKind {
    fileprivate var generated: SweetbriarCore.BodyKind {
        switch self {
        case .sanitizedHtml: .sanitizedHtml
        case .plainText: .plainText
        case .source: .source
        }
    }

    fileprivate init(_ generated: SweetbriarCore.BodyKind) {
        switch generated {
        case .sanitizedHtml: self = .sanitizedHtml
        case .plainText: self = .plainText
        case .source: self = .source
        }
    }
}

extension RenderWarningCode {
    fileprivate init(_ generated: SweetbriarCore.RenderWarningCode) {
        switch generated {
        case .partialParse: self = .partialParse
        case .charsetReplaced: self = .charsetReplaced
        case .unsupportedContent: self = .unsupportedContent
        case .missingCid: self = .missingCid
        case .ambiguousCid: self = .ambiguousCid
        case .unsafeUrl: self = .unsafeUrl
        case .renditionLimit: self = .renditionLimit
        case .countSaturated: self = .countSaturated
        }
    }
}

extension BodyLease {
    fileprivate init(_ generated: SweetbriarCore.BodyLease) {
        self.init(
            leaseId: generated.leaseId, kind: BodyKind(generated.kind),
            filePath: generated.filePath, byteLength: generated.byteLength,
            remoteContentBlocked: generated.remoteContentBlocked,
            warnings: generated.warnings.map {
                RenderWarning(code: RenderWarningCode($0.code), partId: $0.partId)
            },
            manifestPath: generated.manifestPath
        )
    }
}

extension AttachmentLease {
    fileprivate init(_ generated: SweetbriarCore.AttachmentLease) {
        self.init(
            leaseId: generated.leaseId, filePath: generated.filePath,
            byteLength: generated.byteLength, partId: generated.partId,
            displayName: generated.displayName, mediaType: generated.mediaType
        )
    }
}
