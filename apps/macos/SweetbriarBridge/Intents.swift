// Parked and quarantined intents (spec/07 `intents`; spec/02
// "Intents"): what the Activity window lists, with the health actions
// the model runs. The bridge's own value types, mirroring the core's
// field by field, so nothing generated crosses into the app.

import Foundation
import SweetbriarCore

/// One parked or quarantined intent (spec/02).
public struct Intent: Sendable, Equatable {
    public var intentId: String
    public var accountId: String
    public var kind: IntentKind
    public var state: IntentState
    /// The target occurrence ids `target_json` holds.
    public var targetOccurrenceIds: [Int64]
    /// Set on a quarantined intent.
    public var quarantineReason: QuarantineReason?
    /// A parked intent's terminal error, when one was recorded.
    public var lastError: String?
    public var effect: IntentEffect
    public var createdAt: Date

    public init(
        intentId: String, accountId: String, kind: IntentKind, state: IntentState,
        targetOccurrenceIds: [Int64], quarantineReason: QuarantineReason?,
        lastError: String?, effect: IntentEffect, createdAt: Date
    ) {
        self.intentId = intentId
        self.accountId = accountId
        self.kind = kind
        self.state = state
        self.targetOccurrenceIds = targetOccurrenceIds
        self.quarantineReason = quarantineReason
        self.lastError = lastError
        self.effect = effect
        self.createdAt = createdAt
    }

    init(_ value: SweetbriarCore.Intent) {
        self.init(
            intentId: value.intentId,
            accountId: value.accountId,
            kind: IntentKind(value.kind),
            state: IntentState(value.state),
            targetOccurrenceIds: value.targetOccurrenceIds,
            quarantineReason: value.quarantineReason.map(QuarantineReason.init),
            lastError: value.lastError,
            effect: IntentEffect(value.effect),
            createdAt: Date.epochMilliseconds(value.createdAtMs)
        )
    }
}

/// What the intent does (spec/02 `intents.op`).
public enum IntentKind: Sendable, Equatable {
    case setFlags
    case move
    case copy
    case deleteToTrash
    case removeLabel
    /// Reserved, not produced in the pilot (spec/02).
    case expungeScoped
    /// Reserved, not produced in the pilot (spec/02).
    case append
    /// Reserved, not produced in the pilot; sends use send attempts.
    case submit

    init(_ value: SweetbriarCore.IntentKind) {
        switch value {
        case .setFlags: self = .setFlags
        case .move: self = .move
        case .copy: self = .copy
        case .deleteToTrash: self = .deleteToTrash
        case .removeLabel: self = .removeLabel
        case .expungeScoped: self = .expungeScoped
        case .append: self = .append
        case .submit: self = .submit
        }
    }
}

/// The states the health surface lists (spec/02).
public enum IntentState: Sendable, Equatable {
    case parked
    case quarantined

    init(_ value: SweetbriarCore.IntentState) {
        switch value {
        case .parked: self = .parked
        case .quarantined: self = .quarantined
        }
    }
}

/// The intent's effect as stored.
public enum IntentEffect: Sendable, Equatable {
    case setFlags(add: [String], remove: [String])
    case toMailbox(mailboxId: Int64)
    case deleteToTrash
    case removeLabel(mailboxId: Int64)
    /// A reserved operation, not produced in the pilot (spec/02).
    case reserved(argsJson: String)

    init(_ value: SweetbriarCore.IntentEffect) {
        switch value {
        case .setFlags(let add, let remove):
            self = .setFlags(add: add, remove: remove)
        case .toMailbox(let mailboxId):
            self = .toMailbox(mailboxId: mailboxId)
        case .deleteToTrash:
            self = .deleteToTrash
        case .removeLabel(let mailboxId):
            self = .removeLabel(mailboxId: mailboxId)
        case .reserved(let argsJson):
            self = .reserved(argsJson: argsJson)
        }
    }
}

/// Why a quarantined intent waits for the user (spec/02).
public enum QuarantineReason: Sendable, Equatable {
    case epochChange
    case restore
    case ambiguousMatch
    case configChange
    case breaker

    init(_ value: SweetbriarCore.QuarantineReason) {
        switch value {
        case .epochChange: self = .epochChange
        case .restore: self = .restore
        case .ambiguousMatch: self = .ambiguousMatch
        case .configChange: self = .configChange
        case .breaker: self = .breaker
        }
    }
}

/// Which parked or quarantined intents a page reads; nil leaves a
/// dimension unfiltered (spec/07 `intents`).
public struct IntentFilter: Sendable, Equatable {
    public var accountId: String?
    public var state: IntentState?

    public init(accountId: String?, state: IntentState?) {
        self.accountId = accountId
        self.state = state
    }

    var generated: SweetbriarCore.IntentFilter {
        let state: SweetbriarCore.IntentState?
        switch self.state {
        case .parked: state = .parked
        case .quarantined: state = .quarantined
        case nil: state = nil
        }
        return SweetbriarCore.IntentFilter(accountId: accountId, state: state)
    }
}

/// One page of parked or quarantined intents, oldest first.
public struct IntentPage: Sendable, Equatable {
    public var rows: [Intent]
    /// The last row's cursor; nil when the page is empty.
    public var after: String?
    public var hasMore: Bool

    public init(rows: [Intent], after: String?, hasMore: Bool) {
        self.rows = rows
        self.after = after
        self.hasMore = hasMore
    }

    init(_ value: SweetbriarCore.IntentPage) {
        self.init(
            rows: value.rows.map(Intent.init),
            after: value.after,
            hasMore: value.hasMore
        )
    }
}
