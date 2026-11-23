// Per-account health (spec/07 `account_health`,
// `subscribe_account_health`; spec/04 "Account states and health"):
// an account's state and condition, its warnings, open breaker events,
// coverage, a Proton account's sync coverage and last sync time. The
// bridge's own value types, mirroring the core's field by field, so
// nothing generated crosses into the app.

import Foundation
import SweetbriarCore

/// One account's health as the Activity window shows it (spec/07).
public struct AccountHealth: Sendable, Equatable {
    public var accountId: String
    public var state: AccountState
    public var stateReason: String?
    public var condition: AccountCondition
    /// Addresses whose keys didn't unlock; empty when none or no session.
    public var warnings: [String]
    /// The account's open breaker events, oldest first.
    public var breakerEvents: [AccountBreakerEvent]
    public var coverage: SearchCoverage
    /// A Proton account's sync coverage; absent for another kind.
    public var proton: ProtonCoverage?
    /// The worker's last successful sync; absent when never synced.
    public var lastSync: Date?

    public init(
        accountId: String, state: AccountState, stateReason: String?,
        condition: AccountCondition, warnings: [String],
        breakerEvents: [AccountBreakerEvent], coverage: SearchCoverage,
        proton: ProtonCoverage? = nil, lastSync: Date?
    ) {
        self.accountId = accountId
        self.state = state
        self.stateReason = stateReason
        self.condition = condition
        self.warnings = warnings
        self.breakerEvents = breakerEvents
        self.coverage = coverage
        self.proton = proton
        self.lastSync = lastSync
    }

    init(_ value: SweetbriarCore.AccountHealth) {
        self.init(
            accountId: value.accountId,
            state: AccountState(value.state),
            stateReason: value.stateReason,
            condition: AccountCondition(value.condition),
            warnings: value.warnings,
            breakerEvents: value.breakerEvents.map(AccountBreakerEvent.init),
            coverage: SearchCoverage(value.coverage),
            proton: value.proton.map(ProtonCoverage.init),
            lastSync: value.lastSyncMs.map(Date.epochMilliseconds)
        )
    }
}

/// A Proton account's listing (spec/02 `proton_accounts.listing_state`).
public enum ProtonListing: Sendable, Equatable {
    case none
    case listing
    case complete

    init(_ value: SweetbriarCore.ProtonListing) {
        switch value {
        case .none: self = .none
        case .listing: self = .listing
        case .complete: self = .complete
        }
    }
}

/// One live mailbox's count against Proton's (spec/04-sync-proton
/// "Coverage").
public struct ProtonMailboxCoverage: Sendable, Equatable {
    public var mailboxId: Int64
    public var name: String
    /// Present occurrences in the mailbox.
    public var local: UInt64
    /// Proton's total from the last finished listing; absent before one.
    public var remoteTotal: UInt64?

    public init(mailboxId: Int64, name: String, local: UInt64, remoteTotal: UInt64?) {
        self.mailboxId = mailboxId
        self.name = name
        self.local = local
        self.remoteTotal = remoteTotal
    }

    init(_ value: SweetbriarCore.ProtonMailboxCoverage) {
        self.init(
            mailboxId: value.mailboxId, name: value.name, local: value.local,
            remoteTotal: value.remoteTotal
        )
    }
}

/// A Proton account's sync coverage (spec/04-sync-proton "Coverage"):
/// what "Proton messages synced" shows.
public struct ProtonCoverage: Sendable, Equatable {
    public var listing: ProtonListing
    /// The running listing is the resync a Proton `Refresh` asked for.
    public var resync: Bool
    /// Live mailboxes, by name.
    public var mailboxes: [ProtonMailboxCoverage]
    public var messages: UInt64
    public var withBody: UInt64
    public var bodyFailures: UInt64

    public init(
        listing: ProtonListing, resync: Bool, mailboxes: [ProtonMailboxCoverage],
        messages: UInt64, withBody: UInt64, bodyFailures: UInt64
    ) {
        self.listing = listing
        self.resync = resync
        self.mailboxes = mailboxes
        self.messages = messages
        self.withBody = withBody
        self.bodyFailures = bodyFailures
    }

    init(_ value: SweetbriarCore.ProtonCoverage) {
        self.init(
            listing: ProtonListing(value.listing), resync: value.resync,
            mailboxes: value.mailboxes.map(ProtonMailboxCoverage.init),
            messages: value.messages, withBody: value.withBody,
            bodyFailures: value.bodyFailures
        )
    }
}

extension Date {
    /// Epoch milliseconds to the instant the app shows; the bridge's
    /// one conversion for the core's `*Ms` fields.
    static func epochMilliseconds(_ millis: Int64) -> Date {
        Date(timeIntervalSince1970: TimeInterval(millis) / 1000)
    }
}

extension AccountState {
    /// Map the generated enum case by case.
    init(_ value: SweetbriarCore.AccountState) {
        switch value {
        case .active: self = .active
        case .paused: self = .paused
        case .needsAttention: self = .needsAttention
        case .disconnected: self = .disconnected
        }
    }
}

/// The condition spec/04's table names: what the user sees for one
/// account's state.
public enum AccountCondition: Sendable, Equatable {
    case paused
    case disconnected
    case signInAgain
    /// Proton asked for human verification; whether the core still
    /// holds its challenge decides Verify… or Try Again (spec/08).
    case verify(challengeHeld: Bool)
    /// Proton refused this version of Sweetbriar, with its message.
    case versionRefused(message: String)
    /// A secret-store pause, with the stored reason.
    case secretStore(reason: String)
    case offline(nextRetry: Date)
    case notAnswering(nextRetry: Date)
    case rateLimited(until: Date)
    case healthy

    init(_ value: SweetbriarCore.AccountCondition) {
        switch value {
        case .paused:
            self = .paused
        case .disconnected:
            self = .disconnected
        case .signInAgain:
            self = .signInAgain
        case .verify(let challengeHeld):
            self = .verify(challengeHeld: challengeHeld)
        case .versionRefused(let message):
            self = .versionRefused(message: message)
        case .secretStore(let reason):
            self = .secretStore(reason: reason)
        case .offline(let nextRetryMs):
            self = .offline(nextRetry: Date.epochMilliseconds(nextRetryMs))
        case .notAnswering(let nextRetryMs):
            self = .notAnswering(nextRetry: Date.epochMilliseconds(nextRetryMs))
        case .rateLimited(let untilMs):
            self = .rateLimited(until: Date.epochMilliseconds(untilMs))
        case .healthy:
            self = .healthy
        }
    }
}

/// One open breaker event as the health surface lists it.
public struct AccountBreakerEvent: Sendable, Equatable {
    public var eventId: String
    public var mailboxId: Int64?
    public var disappeared: UInt64
    public var detectedAt: Date

    public init(eventId: String, mailboxId: Int64?, disappeared: UInt64, detectedAt: Date) {
        self.eventId = eventId
        self.mailboxId = mailboxId
        self.disappeared = disappeared
        self.detectedAt = detectedAt
    }

    init(_ value: SweetbriarCore.AccountBreakerEvent) {
        self.init(
            eventId: value.eventId,
            mailboxId: value.mailboxId,
            disappeared: value.disappeared,
            detectedAt: Date.epochMilliseconds(value.detectedAtMs)
        )
    }
}

/// What the user chose for a breaker event (spec/07 "Safety").
public enum BreakerResolution: Sendable, Equatable {
    /// Apply the disappearances.
    case accept
    /// Leave the messages withheld and keep watching.
    case keep

    var generated: SweetbriarCore.BreakerResolution {
        switch self {
        case .accept: return .accept
        case .keep: return .keep
        }
    }
}
