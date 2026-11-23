// Sending commands and the Out Box across the L1 boundary (spec/07
// "Commands", "Other queries"): the bridge's own types, their mapping
// onto the generated ones, and the six calls the Out Box and composer
// make. App code never imports `SweetbriarCore`.

import Foundation
import SweetbriarCore

/// How the account's Sent copy is produced (spec/02 "Sending").
public enum SentCopy: Sendable, Equatable {
    case serverSaves
    case pending
    case filed
    case failed

    /// Map the generated enum case by case.
    init(_ g: SweetbriarCore.SentCopy) {
        switch g {
        case .serverSaves: self = .serverSaves
        case .pending: self = .pending
        case .filed: self = .filed
        case .failed: self = .failed
        }
    }
}

/// A recipient's address field (spec/02 `send_recipients.kind`).
public enum RecipientKind: Sendable, Equatable {
    case to
    case cc
    case bcc

    /// Map the generated enum case by case.
    init(_ g: SweetbriarCore.RecipientKind) {
        switch g {
        case .to: self = .to
        case .cc: self = .cc
        case .bcc: self = .bcc
        }
    }
}

/// A recipient's stored state (spec/02 `send_recipients.state`).
public enum RecipientState: Sendable, Equatable {
    case pending
    case rcptAccepted
    case rejectedPermanent
    case rejectedTransient
    case sent
    case unsent
    case unknown

    /// Map the generated enum case by case.
    init(_ g: SweetbriarCore.RecipientState) {
        switch g {
        case .pending: self = .pending
        case .rcptAccepted: self = .rcptAccepted
        case .rejectedPermanent: self = .rejectedPermanent
        case .rejectedTransient: self = .rejectedTransient
        case .sent: self = .sent
        case .unsent: self = .unsent
        case .unknown: self = .unknown
        }
    }
}

/// One recipient on an Out Box row (spec/07): the address as first
/// written, its field and its stored state.
public struct OutBoxRecipient: Sendable, Equatable {
    public var address: String
    public var kind: RecipientKind
    public var state: RecipientState

    public init(address: String, kind: RecipientKind, state: RecipientState) {
        self.address = address
        self.kind = kind
        self.state = state
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.OutBoxRecipient) {
        address = g.address
        kind = RecipientKind(g.kind)
        state = RecipientState(g.state)
    }
}

/// One Out Box row (spec/07 `out_box()`): the attempt's frozen
/// revision subject, recipients, state and stored times, plus the
/// Sent-copy reconciliation and the Proton evidence observed since
/// launch (spec/05 step 6).
public struct OutBoxRow: Sendable, Equatable {
    public var attemptId: String
    public var draftId: String
    public var accountId: String
    public var identityId: String
    /// The sending address the attempt froze (spec/07 `out_box()`).
    public var envelopeFrom: String
    public var subject: String
    public var recipients: [OutBoxRecipient]
    public var state: AttemptState
    public var createdAtMs: Int64
    public var updatedAtMs: Int64
    public var holdUntilMs: Int64?
    public var resolvedAtMs: Int64?
    public var sendRequestedAtMs: Int64?
    /// The Proton draft the attempt created (spec/02, 05).
    public var protonDraftId: String?
    /// Proton's final reply, code and message (spec/02, 05).
    public var finalReply: String?
    public var resolution: UnknownResolution?
    public var sentCopy: SentCopy
    /// A `queued` attempt waits for `confirmSendRoute` (spec/05).
    public var waitsForConfirmation: Bool
    /// Whether the Sent copy has appeared (spec/05 "The Sent copy").
    public var sentCopyAppeared: Bool
    /// A soft warning, never a resend (spec/05).
    public var sentCopyWarning: Bool
    /// The Proton evidence observed since launch (spec/05 step 6).
    public var evidence: [String]

    public init(
        attemptId: String, draftId: String, accountId: String, identityId: String,
        envelopeFrom: String, subject: String, recipients: [OutBoxRecipient], state: AttemptState,
        createdAtMs: Int64, updatedAtMs: Int64, holdUntilMs: Int64?,
        resolvedAtMs: Int64?, sendRequestedAtMs: Int64?, protonDraftId: String?,
        finalReply: String?, resolution: UnknownResolution?, sentCopy: SentCopy,
        waitsForConfirmation: Bool, sentCopyAppeared: Bool, sentCopyWarning: Bool,
        evidence: [String]
    ) {
        self.attemptId = attemptId
        self.draftId = draftId
        self.accountId = accountId
        self.identityId = identityId
        self.envelopeFrom = envelopeFrom
        self.subject = subject
        self.recipients = recipients
        self.state = state
        self.createdAtMs = createdAtMs
        self.updatedAtMs = updatedAtMs
        self.holdUntilMs = holdUntilMs
        self.resolvedAtMs = resolvedAtMs
        self.sendRequestedAtMs = sendRequestedAtMs
        self.protonDraftId = protonDraftId
        self.finalReply = finalReply
        self.resolution = resolution
        self.sentCopy = sentCopy
        self.waitsForConfirmation = waitsForConfirmation
        self.sentCopyAppeared = sentCopyAppeared
        self.sentCopyWarning = sentCopyWarning
        self.evidence = evidence
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.OutBoxRow) {
        self.init(
            attemptId: g.attemptId,
            draftId: g.draftId,
            accountId: g.accountId,
            identityId: g.identityId,
            envelopeFrom: g.envelopeFrom,
            subject: g.subject,
            recipients: g.recipients.map(OutBoxRecipient.init),
            state: AttemptState(g.state),
            createdAtMs: g.createdAtMs,
            updatedAtMs: g.updatedAtMs,
            holdUntilMs: g.holdUntilMs,
            resolvedAtMs: g.resolvedAtMs,
            sendRequestedAtMs: g.sendRequestedAtMs,
            protonDraftId: g.protonDraftId,
            finalReply: g.finalReply,
            resolution: g.resolution.map(UnknownResolution.init),
            sentCopy: SentCopy(g.sentCopy),
            waitsForConfirmation: g.waitsForConfirmation,
            sentCopyAppeared: g.sentCopyAppeared,
            sentCopyWarning: g.sentCopyWarning,
            evidence: g.evidence
        )
    }
}

/// The Out Box (spec/07): live attempts, then history, each newest
/// first.
public struct OutBox: Sendable, Equatable {
    public var live: [OutBoxRow]
    public var history: [OutBoxRow]

    public init(live: [OutBoxRow], history: [OutBoxRow]) {
        self.live = live
        self.history = history
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.OutBox) {
        self.init(
            live: g.live.map(OutBoxRow.init),
            history: g.history.map(OutBoxRow.init)
        )
    }
}

/// The cross-account warning for one draft revision (spec/05, 07):
/// the receiving occurrence's account, the route identity's account,
/// and the acknowledgement `send_draft` accepts for exactly that
/// revision's identity and recipients. The acknowledgement is not
/// persisted.
public struct CrossAccountNotice: Sendable, Equatable {
    public var sourceAccountId: String
    public var routeAccountId: String
    public var ack: String

    public init(sourceAccountId: String, routeAccountId: String, ack: String) {
        self.sourceAccountId = sourceAccountId
        self.routeAccountId = routeAccountId
        self.ack = ack
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.CrossAccountNotice) {
        sourceAccountId = g.sourceAccountId
        routeAccountId = g.routeAccountId
        ack = g.ack
    }
}

/// What `sendDraft` answers: its receipt and the attempt id the
/// receipt names (spec/07).
public struct SendQueued: Sendable, Equatable {
    public var receipt: Receipt
    public var attemptId: String

    public init(receipt: Receipt, attemptId: String) {
        self.receipt = receipt
        self.attemptId = attemptId
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.SendQueued) {
        self.init(receipt: Receipt(g.receipt), attemptId: g.attemptId)
    }
}

/// The user's answer to an `outcome_unknown` attempt (spec/05 step 6).
public enum UnknownResolution: Sendable, Equatable {
    case markedSent
    case sendAgain
    case keptAsDraft

    /// Map the generated enum case by case.
    init(_ g: SweetbriarCore.UnknownResolution) {
        switch g {
        case .markedSent: self = .markedSent
        case .sendAgain: self = .sendAgain
        case .keptAsDraft: self = .keptAsDraft
        }
    }

    /// Map the bridge enum onto the generated one.
    var generated: SweetbriarCore.UnknownResolution {
        switch self {
        case .markedSent: return .markedSent
        case .sendAgain: return .sendAgain
        case .keptAsDraft: return .keptAsDraft
        }
    }
}

/// What `resolveUnknownSend` answers (spec/07).
public struct ResolvedUnknown: Sendable, Equatable {
    public var receipt: Receipt
    public var resolution: UnknownResolution
    public var newAttemptId: String?

    public init(receipt: Receipt, resolution: UnknownResolution, newAttemptId: String?) {
        self.receipt = receipt
        self.resolution = resolution
        self.newAttemptId = newAttemptId
    }

    /// Map the generated record field by field.
    init(_ g: SweetbriarCore.ResolvedUnknown) {
        self.init(
            receipt: Receipt(g.receipt),
            resolution: UnknownResolution(g.resolution),
            newAttemptId: g.newAttemptId
        )
    }
}

/// The sending calls the Out Box and composer need (spec/07).
/// `CoreBridge` adopts it; tests substitute a fake.
public protocol SendSource: Sendable {
    /// `send_draft(draft_id, revision_id, acks)` under a shell-made key.
    func sendDraft(
        idempotencyKey: String, draftId: String, revisionId: String, acks: [String]
    ) async throws -> SendQueued
    /// `cancel_send(attempt_id)` under a shell-made key.
    func cancelSend(idempotencyKey: String, attemptId: String) async throws -> Receipt
    /// `resolve_unknown_send(attempt_id, resolution)` under a shell-made key.
    func resolveUnknownSend(
        idempotencyKey: String, attemptId: String, resolution: UnknownResolution
    ) async throws -> ResolvedUnknown
    /// `confirm_send_route(attempt_id)` under a shell-made key.
    func confirmSendRoute(idempotencyKey: String, attemptId: String) async throws -> Receipt
    /// `retry_rejected_recipients(attempt_id)` under a shell-made key.
    func retryRejectedRecipients(idempotencyKey: String, attemptId: String) async throws -> Receipt
    /// `out_box()`: live attempts, then history.
    func outBox() async throws -> OutBox
    /// `cross_account_notice(draft_id, revision_id)`: the cross-account
    /// warning for that revision of a reply or reply-all, or none.
    func crossAccountNotice(
        draftId: String, revisionId: String
    ) async throws -> CrossAccountNotice?
}

