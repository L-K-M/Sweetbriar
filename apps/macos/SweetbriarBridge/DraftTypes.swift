// Bridge-owned draft records and native mappings. The attachment wrapper
// retains the original publication; app code never handles generated types.

import Foundation
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
        receipt = Receipt(g.receipt)
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
        receipt = Receipt(g.receipt)
        revisionId = g.revisionId
    }
}

/// One local draft row for the sidebar, without loading its full body.
public struct DraftSummary: Sendable, Equatable {
    public var draftId: String
    public var state: DraftState
    public var accountId: String?
    public var identityId: String?
    public var subject: String
    public var to: [DraftRecipient]
    public var cc: [DraftRecipient]
    public var bcc: [DraftRecipient]
    public var updatedAtMs: Int64
    public var attempt: LiveAttempt?

    public init(draftId: String, state: DraftState, accountId: String?, identityId: String?,
                subject: String, to: [DraftRecipient], cc: [DraftRecipient],
                bcc: [DraftRecipient], updatedAtMs: Int64, attempt: LiveAttempt?) {
        self.draftId = draftId
        self.state = state
        self.accountId = accountId
        self.identityId = identityId
        self.subject = subject
        self.to = to
        self.cc = cc
        self.bcc = bcc
        self.updatedAtMs = updatedAtMs
        self.attempt = attempt
    }

    init(_ g: SweetbriarCore.DraftSummary) {
        draftId = g.draftId
        state = DraftState(g.state)
        accountId = g.accountId
        identityId = g.identityId
        subject = g.subject
        to = g.to.map(DraftRecipient.init)
        cc = g.cc.map(DraftRecipient.init)
        bcc = g.bcc.map(DraftRecipient.init)
        updatedAtMs = g.updatedAtMs
        attempt = g.attempt.map(LiveAttempt.init)
    }
}
