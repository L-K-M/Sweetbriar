// The Activity window's strings (spec/08 "Activity / account health";
// spec/04 "Account states and health"). Pure text, free of AppKit, so
// the bridge's tests pin a locale and a clock.
//
//   Offline                          Next retry in 30 seconds
//   Proton asked Sweetbriar to slow down   Until 2:37 PM
//   Review 12 disappeared messages
//   Proton messages synced           1,210 messages, 1,204 with bodies
//   Inbox                            1,204 of 1,210

import Foundation

/// One row's text: a title and an optional detail.
public struct ActivityLine: Sendable, Equatable {
    public var title: String
    public var detail: String?

    public init(title: String, detail: String? = nil) {
        self.title = title
        self.detail = detail
    }
}

/// The window's presentation: every string the Activity surface shows,
/// formatted with one locale and time zone. Tests pin both and pass the
/// instant a relative time is told from.
public struct ActivityPresentation: Sendable {
    private let formatters: Formatters

    public init(locale: Locale = .current, timeZone: TimeZone = .current) {
        let timeFormatter = DateFormatter()
        timeFormatter.locale = locale
        timeFormatter.timeZone = timeZone
        timeFormatter.dateStyle = .none
        timeFormatter.timeStyle = .short
        let relativeFormatter = RelativeDateTimeFormatter()
        relativeFormatter.locale = locale
        relativeFormatter.unitsStyle = .full
        let countFormatter = NumberFormatter()
        countFormatter.locale = locale
        countFormatter.numberStyle = .decimal
        self.formatters = Formatters(
            time: timeFormatter, relative: relativeFormatter, count: countFormatter
        )
    }

    /// An account's condition as spec/04's table tells it. `lastSync`
    /// only matters for a healthy account: "Last synced …" while the
    /// worker has synced once, "Not synced since launch" before that.
    public func condition(
        _ condition: AccountCondition, lastSync: Date?, now: Date
    ) -> ActivityLine {
        switch condition {
        case .paused:
            return ActivityLine(title: "Paused")
        case .disconnected:
            return ActivityLine(title: "Disconnected")
        case .signInAgain:
            return ActivityLine(title: "Sign in again")
        case .verify:
            return ActivityLine(title: "Proton asks you to verify")
        case .versionRefused(let message):
            return ActivityLine(
                title: "Proton refused this version of Sweetbriar", detail: message
            )
        case .secretStore(let reason):
            return ActivityLine(title: "Can't reach the secret store", detail: reason)
        case .offline(let nextRetry):
            return ActivityLine(
                title: "Offline", detail: "Next retry \(relative(nextRetry, now: now))"
            )
        case .notAnswering(let nextRetry):
            return ActivityLine(
                title: "Proton isn't answering",
                detail: "Next retry \(relative(nextRetry, now: now))"
            )
        case .rateLimited(let until):
            return ActivityLine(
                title: "Proton asked Sweetbriar to slow down",
                detail: "Until \(time(until))"
            )
        case .healthy:
            guard let lastSync else {
                return ActivityLine(title: "Not synced since launch")
            }
            return ActivityLine(title: "Last synced \(relative(lastSync, now: now))")
        }
    }

    /// An open breaker event's line: the user reviews what disappeared.
    public func breakerEvent(_ event: AccountBreakerEvent) -> String {
        "Review \(event.disappeared) disappeared "
            + (event.disappeared == 1 ? "message" : "messages")
    }

    /// A Proton account's sync coverage (spec/04-sync-proton "Coverage"):
    /// "Proton messages synced" once a listing completed, "Proton asked
    /// Sweetbriar to resync" while a refresh's listing runs, and "Listing
    /// Proton messages" during the first one. The detail counts the
    /// messages, those with a body and those whose body failed.
    public func protonCoverage(_ coverage: ProtonCoverage) -> ActivityLine {
        let title: String
        if coverage.resync {
            title = "Proton asked Sweetbriar to resync"
        } else if coverage.listing == .complete {
            title = "Proton messages synced"
        } else {
            title = "Listing Proton messages"
        }
        var detail = "\(count(coverage.messages)) "
            + (coverage.messages == 1 ? "message" : "messages")
            + ", \(count(coverage.withBody)) with bodies"
        if coverage.bodyFailures > 0 {
            detail += ", \(count(coverage.bodyFailures)) couldn't be read"
        }
        return ActivityLine(title: title, detail: detail)
    }

    /// One mailbox against Proton: "1,204 of 1,210", or the local count
    /// alone before a listing finished.
    public func protonMailbox(_ mailbox: ProtonMailboxCoverage) -> ActivityLine {
        let local = count(mailbox.local)
        guard let total = mailbox.remoteTotal else {
            return ActivityLine(title: mailbox.name, detail: local)
        }
        return ActivityLine(title: mailbox.name, detail: "\(local) of \(count(total))")
    }

    /// Whether the row offers "Sign in again": a disconnected account
    /// and a refused refresh both sign in again (spec/07).
    public static func offersSignInAgain(_ condition: AccountCondition) -> Bool {
        switch condition {
        case .signInAgain, .disconnected:
            return true
        default:
            return false
        }
    }

    /// An intent row: what it does, then its state and why it waits.
    public func intent(_ intent: Intent) -> ActivityLine {
        let state: String
        switch intent.state {
        case .parked:
            state = intent.lastError.map { "Parked · \($0)" } ?? "Parked"
        case .quarantined:
            state = "Quarantined · \(Self.quarantineReason(intent.quarantineReason))"
        }
        return ActivityLine(title: Self.intentKind(intent.kind), detail: state)
    }

    /// A Recovery row: its state, the mailbox the last occurrence was
    /// in and when it was last seen.
    public func recovery(_ row: Recovery, now: Date) -> ActivityLine {
        ActivityLine(
            title: Self.recoveryState(row.state),
            detail: "\(row.mailboxName) · Last seen \(relative(row.lastSeenAt, now: now))"
        )
    }

    private func time(_ date: Date) -> String {
        formatters.time.string(from: date)
    }

    private func relative(_ date: Date, now: Date) -> String {
        formatters.relative.localizedString(for: date, relativeTo: now)
    }

    private func count(_ value: UInt64) -> String {
        formatters.count.string(from: NSNumber(value: value)) ?? String(value)
    }

    /// What the intent does, as the list names it.
    private static func intentKind(_ kind: IntentKind) -> String {
        switch kind {
        case .setFlags: return "Set flags"
        case .move: return "Move"
        case .copy: return "Copy"
        case .deleteToTrash: return "Delete to trash"
        case .removeLabel: return "Remove label"
        case .expungeScoped: return "Expunge"
        case .append: return "Append"
        case .submit: return "Submit"
        }
    }

    private static func quarantineReason(_ reason: QuarantineReason?) -> String {
        switch reason {
        case .epochChange: return "Server epoch changed"
        case .restore: return "Restored archive"
        case .ambiguousMatch: return "Ambiguous match"
        case .configChange: return "Account configuration changed"
        case .breaker: return "Disappeared messages"
        case nil: return "Unknown reason"
        }
    }

    private static func recoveryState(_ state: RecoveryState) -> String {
        switch state {
        case .vanished: return "Vanished"
        case .epochStale: return "Server epoch changed"
        case .superseded: return "Superseded"
        }
    }

    /// Immutable formatters, built once per presentation value.
    private struct Formatters: @unchecked Sendable {
        let time: DateFormatter
        let relative: RelativeDateTimeFormatter
        let count: NumberFormatter
    }
}
