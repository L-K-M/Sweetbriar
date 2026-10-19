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

extension CoreBridge: ListWindowSource {}

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
