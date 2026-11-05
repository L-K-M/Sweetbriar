// `DraftModel`: one draft as the compose window drives it (spec/02,
// spec/05, spec/07; plan M1.9b2c). The bridge owns the DTOs and this
// session: app code never sees a generated `SweetbriarCore` type.
// `load` reads the draft, `save` compares against the displayed base
// revision and exposes a typed `Conflict`, `discard` marks it discarded,
// and `stop` ends the session without cancelling a begun command.

import Foundation
import Observation
import SweetbriarCore

// MARK: - Bridge-owned draft DTOs
/// A draft's state (spec/02 `drafts.state`).
public enum DraftState: Sendable, Equatable {
    case editing, queued, sent, discarded

    /// Map the generated enum case by case.
    init(_ g: SweetbriarCore.DraftState) {
        switch g {
        case .editing: self = .editing
        case .queued: self = .queued
        case .sent: self = .sent
        case .discarded: self = .discarded
        }
    }
}

/// A draft's compose format (spec/02, spec/05 "Formats").
public enum ComposeFormat: Sendable, Equatable {
    case plain, markdown

    /// Map the generated enum case by case.
    init(_ g: SweetbriarCore.ComposeFormat) {
        switch g {
        case .plain: self = .plain
        case .markdown: self = .markdown
        }
    }

    /// Map the bridge enum onto the generated one.
    var generated: SweetbriarCore.ComposeFormat {
        switch self {
        case .plain: return .plain
        case .markdown: return .markdown
        }
    }
}

/// A send attempt's state word (spec/02 `send_attempts`); a draft
/// reports only a live one (spec/05).
public enum AttemptState: Sendable, Equatable {
    case held, queued, submitting, accepted, failed, outcomeUnknown, cancelled

    /// Map the generated enum case by case.
    init(_ g: SweetbriarCore.AttemptState) {
        switch g {
        case .held: self = .held
        case .queued: self = .queued
        case .submitting: self = .submitting
        case .accepted: self = .accepted
        case .failed: self = .failed
        case .outcomeUnknown: self = .outcomeUnknown
        case .cancelled: self = .cancelled
        }
    }
}

/// One address slot on a revision (spec/02 `{address, name}`).
public struct DraftRecipient: Sendable, Equatable {
    public var address: String
    public var name: String?

    public init(address: String, name: String?) {
        self.address = address
        self.name = name
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.DraftRecipient) {
        address = g.address
        name = g.name
    }

    /// Map the bridge record onto the generated one.
    var generated: SweetbriarCore.DraftRecipient {
        SweetbriarCore.DraftRecipient(address: address, name: name)
    }
}

/// An attachment reference a published object leaves on a revision
/// (spec/02 `attachments_json`).
public struct DraftAttachmentRef: Sendable, Equatable {
    public var objectSha256: String
    public var filename: String
    public var mediaType: String
    public var size: UInt64

    public init(objectSha256: String, filename: String, mediaType: String, size: UInt64) {
        self.objectSha256 = objectSha256
        self.filename = filename
        self.mediaType = mediaType
        self.size = size
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.DraftAttachmentRef) {
        objectSha256 = g.objectSha256
        filename = g.filename
        mediaType = g.mediaType
        size = g.size
    }

    /// Map the bridge record onto the generated one.
    var generated: SweetbriarCore.DraftAttachmentRef {
        SweetbriarCore.DraftAttachmentRef(
            objectSha256: objectSha256, filename: filename,
            mediaType: mediaType, size: size
        )
    }
}

/// A draft's live send attempt (spec/05).
public struct LiveAttempt: Sendable, Equatable {
    public var attemptId: String
    public var state: AttemptState

    public init(attemptId: String, state: AttemptState) {
        self.attemptId = attemptId
        self.state = state
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.LiveAttempt) {
        attemptId = g.attemptId
        state = AttemptState(g.state)
    }
}

/// One immutable draft revision (spec/02 `draft_revisions`).
public struct DraftRevision: Sendable, Equatable {
    public var revisionId: String
    public var seq: Int64
    public var baseRevisionId: String?
    public var accountId: String?
    public var identityId: String?
    public var format: ComposeFormat
    public var plainFlowed: Bool
    public var subject: String
    public var to: [DraftRecipient]
    public var cc: [DraftRecipient]
    public var bcc: [DraftRecipient]
    public var bodySource: String
    public var attachments: [DraftAttachmentRef]
    public var createdAtMs: Int64

    public init(
        revisionId: String, seq: Int64, baseRevisionId: String?,
        accountId: String?, identityId: String?, format: ComposeFormat,
        plainFlowed: Bool, subject: String, to: [DraftRecipient],
        cc: [DraftRecipient], bcc: [DraftRecipient], bodySource: String,
        attachments: [DraftAttachmentRef], createdAtMs: Int64
    ) {
        self.revisionId = revisionId
        self.seq = seq
        self.baseRevisionId = baseRevisionId
        self.accountId = accountId
        self.identityId = identityId
        self.format = format
        self.plainFlowed = plainFlowed
        self.subject = subject
        self.to = to
        self.cc = cc
        self.bcc = bcc
        self.bodySource = bodySource
        self.attachments = attachments
        self.createdAtMs = createdAtMs
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.DraftRevision) {
        revisionId = g.revisionId
        seq = g.seq
        baseRevisionId = g.baseRevisionId
        accountId = g.accountId
        identityId = g.identityId
        format = ComposeFormat(g.format)
        plainFlowed = g.plainFlowed
        subject = g.subject
        to = g.to.map(DraftRecipient.init)
        cc = g.cc.map(DraftRecipient.init)
        bcc = g.bcc.map(DraftRecipient.init)
        bodySource = g.bodySource
        attachments = g.attachments.map(DraftAttachmentRef.init)
        createdAtMs = g.createdAtMs
    }
}

/// A draft and its current revision (spec/07 `draft(draft_id)`).
public struct Draft: Sendable, Equatable {
    public var draftId: String
    public var state: DraftState
    public var replyToOccurrenceId: Int64?
    public var forwardOfMessageId: Int64?
    public var revision: DraftRevision?
    public var attempt: LiveAttempt?
    public var createdAtMs: Int64
    public var updatedAtMs: Int64

    public init(
        draftId: String, state: DraftState, replyToOccurrenceId: Int64?,
        forwardOfMessageId: Int64?, revision: DraftRevision?,
        attempt: LiveAttempt?, createdAtMs: Int64, updatedAtMs: Int64
    ) {
        self.draftId = draftId
        self.state = state
        self.replyToOccurrenceId = replyToOccurrenceId
        self.forwardOfMessageId = forwardOfMessageId
        self.revision = revision
        self.attempt = attempt
        self.createdAtMs = createdAtMs
        self.updatedAtMs = updatedAtMs
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.Draft) {
        draftId = g.draftId
        state = DraftState(g.state)
        replyToOccurrenceId = g.replyToOccurrenceId
        forwardOfMessageId = g.forwardOfMessageId
        revision = g.revision.map(DraftRevision.init)
        attempt = g.attempt.map(LiveAttempt.init)
        createdAtMs = g.createdAtMs
        updatedAtMs = g.updatedAtMs
    }
}

/// `create_draft`'s context (spec/07): new, or the source occurrence
/// a reply, reply-all or forward answers.
public enum DraftSource: Sendable, Equatable {
    case new
    case reply(occurrenceId: Int64)
    case replyAll(occurrenceId: Int64)
    case forward(occurrenceId: Int64)

    /// Map the bridge enum onto the generated one.
    var generated: SweetbriarCore.DraftSource {
        switch self {
        case .new: return .new
        case .reply(let id): return .reply(occurrenceId: id)
        case .replyAll(let id): return .replyAll(occurrenceId: id)
        case .forward(let id): return .forward(occurrenceId: id)
        }
    }
}

/// A published attachment's opaque handle (spec/02): the wrapper
/// retains the native publication and its reservation; never rebuilt.
public final class DraftAttachmentObject: Sendable, Equatable {
    /// The native publication handle; only `publishDraftAttachment`
    /// makes one.
    let native: SweetbriarCore.DraftAttachmentObject

    /// The published object's SHA-256: 64 lowercase hex digits.
    public var objectSha256: String { native.objectSha256() }
    /// The published object's length in bytes.
    public var byteLength: UInt64 { native.byteLength() }

    init(_ native: SweetbriarCore.DraftAttachmentObject) {
        self.native = native
    }

    public static func == (lhs: DraftAttachmentObject, rhs: DraftAttachmentObject) -> Bool {
        lhs.objectSha256 == rhs.objectSha256 && lhs.byteLength == rhs.byteLength
    }
}

/// One attachment in a save's content: a newly published object, or
/// an unchanged reference from the base revision (spec/02).
public enum DraftAttachment: Sendable, Equatable {
    case new(object: DraftAttachmentObject, filename: String, mediaType: String)
    case retained(DraftAttachmentRef)

    /// Map the bridge enum onto the generated one; a new object keeps
    /// its native handle.
    var generated: SweetbriarCore.DraftAttachment {
        switch self {
        case .new(let object, let filename, let mediaType):
            return .new(object: object.native, filename: filename, mediaType: mediaType)
        case .retained(let reference):
            return .retained(reference.generated)
        }
    }
}

/// One save's content (spec/07 `save_draft`'s `content`): everything
/// a revision records but its ids.
public struct DraftContent: Sendable, Equatable {
    /// The sending identity the route snapshots (`nil` keeps the
    /// revision unrouted: a local draft, spec/02).
    public var identityId: String?
    public var format: ComposeFormat
    /// `false` is the per-draft literal plain-text option (spec/05).
    public var plainFlowed: Bool
    public var subject: String
    public var to: [DraftRecipient]
    public var cc: [DraftRecipient]
    public var bcc: [DraftRecipient]
    /// Markdown or plain source as composed (spec/05).
    public var bodySource: String
    /// The published objects this revision references.
    public var attachments: [DraftAttachment]

    public init(
        identityId: String?, format: ComposeFormat, plainFlowed: Bool,
        subject: String, to: [DraftRecipient], cc: [DraftRecipient],
        bcc: [DraftRecipient], bodySource: String, attachments: [DraftAttachment]
    ) {
        self.identityId = identityId
        self.format = format
        self.plainFlowed = plainFlowed
        self.subject = subject
        self.to = to
        self.cc = cc
        self.bcc = bcc
        self.bodySource = bodySource
        self.attachments = attachments
    }

    /// The revision's content as an editable draft: attachments are
    /// retained references until the caller publishes new ones.
    init(_ revision: DraftRevision) {
        identityId = revision.identityId
        format = revision.format
        plainFlowed = revision.plainFlowed
        subject = revision.subject
        to = revision.to
        cc = revision.cc
        bcc = revision.bcc
        bodySource = revision.bodySource
        attachments = revision.attachments.map { .retained($0) }
    }

    /// A new draft's content (spec/05): plain, flowed, unrouted.
    static func empty(identityId: String? = nil) -> DraftContent {
        DraftContent(
            identityId: identityId, format: .plain, plainFlowed: true,
            subject: "", to: [], cc: [], bcc: [], bodySource: "", attachments: []
        )
    }

    /// Map the bridge record onto the generated one, spending no
    /// reservation here: the native save's transaction does.
    var generated: SweetbriarCore.DraftContent {
        SweetbriarCore.DraftContent(
            identityId: identityId, format: format.generated,
            plainFlowed: plainFlowed, subject: subject,
            to: to.map { $0.generated }, cc: cc.map { $0.generated },
            bcc: bcc.map { $0.generated }, bodySource: bodySource,
            attachments: attachments.map { $0.generated }
        )
    }
}

/// What `create_draft` answers: its receipt and the draft id.
public struct DraftCreated: Sendable, Equatable {
    public var receipt: Receipt
    public var draftId: String

    public init(receipt: Receipt, draftId: String) {
        self.receipt = receipt
        self.draftId = draftId
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.DraftCreated) {
        receipt = Receipt(
            command: g.receipt.command, resultJson: g.receipt.resultJson,
            createdAtMs: g.receipt.createdAtMs
        )
        draftId = g.draftId
    }
}

/// What `save_draft` answers: its receipt and the revision id.
public struct DraftSaved: Sendable, Equatable {
    public var receipt: Receipt
    public var revisionId: String

    public init(receipt: Receipt, revisionId: String) {
        self.receipt = receipt
        self.revisionId = revisionId
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.DraftSaved) {
        receipt = Receipt(
            command: g.receipt.command, resultJson: g.receipt.resultJson,
            createdAtMs: g.receipt.createdAtMs
        )
        revisionId = g.revisionId
    }
}

// MARK: - Model

/// The draft queries and commands a compose session needs (spec/07).
/// `CoreBridge` adopts it; tests substitute a fake. Keys stay with the
/// caller: a retry of an ambiguous save replays its receipt.
public protocol DraftStore: Sendable {
    func draft(id: String) async throws -> Draft
    func saveDraft(
        idempotencyKey: String, draftId: String,
        baseRevisionId: String?, content: DraftContent
    ) async throws -> DraftSaved
    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt
}

extension CoreBridge: DraftStore {}

/// One draft's compose session (spec/02, spec/05; plan M1.9b2c):
/// load the displayed revision, edit, save over its base revision,
/// reload without losing unsaved edits, discard, and stop without
/// cancelling a begun native command.
@MainActor
@Observable
public final class DraftModel {
    /// A same-base save refusal (spec/02, spec/07): the unsaved
    /// content and the base it was attempted over, so M1.9c can offer
    /// keep-both.
    public struct Conflict: Sendable, Equatable {
        public var draftId: String
        public var baseRevisionId: String?
        public var content: DraftContent
        public var detail: String

        public init(
            draftId: String, baseRevisionId: String?, content: DraftContent, detail: String
        ) {
            self.draftId = draftId
            self.baseRevisionId = baseRevisionId
            self.content = content
            self.detail = detail
        }
    }

    /// The draft's sending route (spec/02): no revision yet, a
    /// revision with no identity (a local draft), or the selected
    /// identity's account and id.
    public enum Route: Sendable, Equatable {
        case noRevision
        case unrouted
        case routed(accountId: String, identityId: String)
    }

    public let draftId: String
    /// The last loaded draft record: state, timestamps and any live
    /// attempt.
    public private(set) var draft: Draft?
    /// The last loaded revision; its content is what the editor shows
    /// when the model is clean.
    public private(set) var revision: DraftRevision?
    /// The current editable content; nil until a load succeeds.
    public private(set) var content: DraftContent?
    /// The revision id the next save compares against (spec/02).
    public private(set) var baseRevisionId: String?
    /// True when `content` differs from the displayed base.
    public private(set) var isDirty = false
    public private(set) var isSaving = false
    public private(set) var isLoading = false
    /// The last same-base refusal, until a reload or a successful save.
    public private(set) var conflict: Conflict?
    /// The last read, save or discard failure.
    public private(set) var failure: (any Error)?
    public private(set) var isDiscarded = false
    /// True once `stop` ran; completions are ignored but a begun command still commits.
    public private(set) var isStopped = false

    /// The route the displayed revision carries.
    public var route: Route {
        guard let revision else { return .noRevision }
        guard let identityId = revision.identityId,
              let accountId = revision.accountId else { return .unrouted }
        return .routed(accountId: accountId, identityId: identityId)
    }

    /// Whether a save can run now.
    public var canSave: Bool {
        content != nil && !isSaving && !isStopped && !isDiscarded
    }

    private let source: any DraftStore
    @ObservationIgnored private var loadTicket: UInt64 = 0
    @ObservationIgnored private var saveTicket: UInt64 = 0
    @ObservationIgnored private var userEditVersion: UInt64 = 0
    @ObservationIgnored private var lastSave: (key: String, content: DraftContent, baseRevisionId: String?)?

    public init(source: any DraftStore, draftId: String) {
        self.source = source
        self.draftId = draftId
    }

    /// Read the draft and show its current revision, discarding unsaved content.
    public func load() async {
        await read(mode: .replace)
    }

    /// Read again: a clean editor follows the revision; an unsaved editor
    /// keeps its content, including an edit made while the read was in flight.
    public func reload() async {
        await read(mode: .preserveIfDirty)
    }

    /// Replace the editable content; every edit bumps the version a
    /// save compares against, so a load or reload never counts as one.
    public func update(_ content: DraftContent) {
        guard !isStopped, !isDiscarded, self.content != nil else { return }
        self.content = content
        isDirty = true
        userEditVersion &+= 1
    }

    /// Save `content` over the displayed base revision. A retry of the
    /// same content and base replays its receipt, so a spent attachment
    /// reservation cannot block it; an edit during the save survives.
    public func save() async {
        guard !isStopped, !isSaving, !isDiscarded, let content else { return }
        let base = baseRevisionId
        let key = saveKey(for: content, base: base)
        let ticket = saveTicket &+ 1
        saveTicket = ticket
        let version = userEditVersion
        isSaving = true
        conflict = nil
        failure = nil
        defer { if !isStopped { isSaving = false } }
        do {
            let saved = try await source.saveDraft(
                idempotencyKey: key, draftId: draftId, baseRevisionId: base, content: content
            )
            guard !isStopped, ticket == saveTicket else { return }
            baseRevisionId = saved.revisionId
            isDirty = version == userEditVersion ? false : true
            // Refresh the displayed revision behind the receipt; an
            // unsaved editor keeps its newer content. A new attachment
            // handle the save spent stays in that content; M1.9c must
            // reconcile it with the reloaded revision's retained
            // reference before the next save. No handle is rebuilt here.
            await read(mode: .preserveIfDirty)
        } catch let error as CoreFailure {
            guard case .conflict(let detail) = error else {
                guard !isStopped, ticket == saveTicket else { return }
                failure = error
                return
            }
            guard !isStopped, ticket == saveTicket else { return }
            conflict = Conflict(
                draftId: draftId, baseRevisionId: base, content: content, detail: detail
            )
        } catch is CancellationError {
            // A cancelled-before-begin save commits nothing. A begun
            // command runs to completion and returns its receipt, so
            // this path has no result to apply.
        } catch {
            guard !isStopped, ticket == saveTicket else { return }
            failure = error
        }
    }

    /// Mark the draft discarded under a fresh idempotency key.
    public func discard() async {
        guard !isStopped, !isDiscarded, !isSaving else { return }
        conflict = nil
        failure = nil
        do {
            _ = try await source.discardDraft(
                idempotencyKey: UUID().uuidString, draftId: draftId
            )
            guard !isStopped else { return }
            isDiscarded = true
        } catch is CancellationError {
        } catch {
            guard !isStopped else { return }
            failure = error
        }
    }

    /// End the session: completions stop publishing, but a begun command commits.
    public func stop() {
        guard !isStopped else { return }
        isStopped = true
        loadTicket &+= 1
        saveTicket &+= 1
    }

    /// One read at a time; a newer read or `stop` supersedes this one.
    /// A reload preserves content that became dirty while it was in
    /// flight, so a completion never clears a newer edit.
    private func read(mode: ReadMode) async {
        guard !isStopped else { return }
        isLoading = true
        failure = nil
        conflict = nil
        let ticket = loadTicket &+ 1
        loadTicket = ticket
        do {
            let draft = try await source.draft(id: draftId)
            guard !isStopped, ticket == loadTicket else { return }
            let preserve = mode == .preserveIfDirty && isDirty
            apply(draft, preservingContent: preserve)
            failure = nil
        } catch is CancellationError {
            // A cancelled read leaves the model as it was.
        } catch {
            guard !isStopped, ticket == loadTicket else { return }
            failure = error
        }
        if ticket == loadTicket, !isStopped { isLoading = false }
    }

    /// What a read does with the editor's content.
    private enum ReadMode {
        /// Show the loaded revision, discarding unsaved content.
        case replace
        /// Follow the loaded revision unless the editor is dirty.
        case preserveIfDirty
    }

    private func apply(_ draft: Draft, preservingContent: Bool) {
        self.draft = draft
        isDiscarded = draft.state == .discarded
        if let revision = draft.revision {
            self.revision = revision
            baseRevisionId = revision.revisionId
            if !preservingContent {
                content = DraftContent(revision)
                isDirty = false
            }
        } else {
            self.revision = nil
            baseRevisionId = nil
            if !preservingContent {
                content = DraftContent.empty()
                isDirty = false
            }
        }
    }

    /// The last key while content and base are unchanged, so an ambiguous
    /// attempt replays its receipt.
    private func saveKey(for content: DraftContent, base: String?) -> String {
        if let lastSave, lastSave.content == content, lastSave.baseRevisionId == base {
            return lastSave.key
        }
        let key = UUID().uuidString
        lastSave = (key, content, base)
        return key
    }
}
