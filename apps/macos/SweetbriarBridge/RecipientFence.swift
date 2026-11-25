// The recipient check (spec/05 "Recipient check", spec/07, spec/08):
// the compose window's view of `recipient_fence` for its current
// revision, the allowance commands, and Settings' allowance list.

import Foundation
import Observation
import SweetbriarCore

/// What `recipient_fence` answers for one revision: whether the check is
/// ready and each blocked recipient. Nothing is blocked before it is
/// ready.
public struct RecipientFence: Sendable, Equatable {
    public var ready: Bool
    public var blocked: [BlockedRecipient]

    public init(ready: Bool, blocked: [BlockedRecipient]) {
        self.ready = ready
        self.blocked = blocked
    }

    init(_ g: SweetbriarCore.RecipientFence) {
        self.init(ready: g.ready, blocked: g.blocked.map(BlockedRecipient.init))
    }
}

/// A blocked recipient: its address as the draft spells it and the
/// sending accounts it has written to.
public struct BlockedRecipient: Sendable, Equatable {
    public var address: String
    public var accountIds: [String]

    public init(address: String, accountIds: [String]) {
        self.address = address
        self.accountIds = accountIds
    }

    init(_ g: SweetbriarCore.BlockedRecipient) {
        self.init(address: g.address, accountIds: g.accountIds)
    }
}

/// One allowance, as Settings → Sending lists it.
public struct RecipientAllowance: Sendable, Equatable {
    public var accountId: String
    public var address: String
    public var createdAtMs: Int64

    public init(accountId: String, address: String, createdAtMs: Int64) {
        self.accountId = accountId
        self.address = address
        self.createdAtMs = createdAtMs
    }

    init(_ g: SweetbriarCore.RecipientAllowance) {
        self.init(accountId: g.accountId, address: g.address, createdAtMs: g.createdAtMs)
    }
}

/// The recipient check's calls (spec/07). Separate from `SendSource`, so
/// a window without it simply shows no check.
public protocol RecipientFenceSource: AnyObject, Sendable {
    func recipientFence(draftId: String, revisionId: String) async throws -> RecipientFence
    func allowRecipient(accountId: String, address: String) async throws -> Receipt
    func removeRecipientAllowance(accountId: String, address: String) async throws -> Receipt
    func recipientAllowances() async throws -> [RecipientAllowance]
}

/// One compose window's recipient check (spec/08 "Recipient check"): the
/// newest answer for the current revision. Rereads of the same revision
/// are allowed, since sender history and readiness change without a
/// compose event; an answer for an older query is dropped.
@MainActor
@Observable
public final class RecipientFenceModel {
    /// The shown answer, or nil before one arrives or after a failure.
    public private(set) var fence: RecipientFence?
    /// The revision `fence` answers for.
    public private(set) var revisionId: String?
    @ObservationIgnored private var ticket: UInt64 = 0

    public init() {}

    /// The blocked recipients to show; none while the check isn't ready.
    public var blocked: [BlockedRecipient] { fence?.blocked ?? [] }
    /// Whether Send must stay disabled.
    public var isBlocking: Bool { !blocked.isEmpty }
    /// Whether to show the not-ready note.
    public var isWaitingForHistory: Bool { fence?.ready == false }

    /// Ask for `revisionId`'s check. A newer query wins. A failed reread
    /// keeps the revision's last answer, so a passing error never lifts a
    /// block; with no answer yet a failure shows nothing, and Send's own
    /// refusal still holds (spec/05).
    public func refresh(source: any RecipientFenceSource, draftId: String, revisionId: String) async {
        ticket &+= 1
        let mine = ticket
        if self.revisionId != revisionId {
            // The previous revision's answer no longer describes this one.
            fence = nil
            self.revisionId = nil
        }
        do {
            let answer = try await source.recipientFence(draftId: draftId, revisionId: revisionId)
            guard mine == ticket else { return }
            fence = answer
            self.revisionId = revisionId
        } catch {
            // Keep what's shown: the prologue already dropped another
            // revision's answer.
        }
    }

    /// Forget the answer, as when the window has no saved revision.
    public func clear() {
        ticket &+= 1
        fence = nil
        revisionId = nil
    }
}
