import Foundation
import Testing

@testable import SweetbriarBridge

@Suite
struct ActivityPresentationTests {
    private let locale = Locale(identifier: "en_US_POSIX")
    private let timeZone = TimeZone(identifier: "UTC")!
    private let now = Date(timeIntervalSince1970: 10_000)
    private var presentation: ActivityPresentation {
        ActivityPresentation(locale: locale, timeZone: timeZone)
    }

    /// The locale's own short time, so the test does not hard-code the
    /// space the formatter puts before AM.
    private func time(_ date: Date) -> String {
        let formatter = DateFormatter()
        formatter.locale = locale
        formatter.timeZone = timeZone
        formatter.dateStyle = .none
        formatter.timeStyle = .short
        return formatter.string(from: date)
    }

    @Test
    func everyConditionMatchesTheSpecTable() {
        let p = presentation
        #expect(p.condition(.paused, lastSync: nil, now: now) == ActivityLine(title: "Paused"))
        #expect(
            p.condition(.disconnected, lastSync: nil, now: now)
                == ActivityLine(title: "Disconnected"))
        #expect(
            p.condition(.signInAgain, lastSync: nil, now: now)
                == ActivityLine(title: "Sign in again"))
        #expect(
            p.condition(.verify, lastSync: nil, now: now)
                == ActivityLine(title: "Proton asks you to verify"))
        #expect(
            p.condition(.versionRefused(message: "2064"), lastSync: nil, now: now)
                == ActivityLine(
                    title: "Proton refused this version of Sweetbriar",
                    detail: "2064"))
        #expect(
            p.condition(.secretStore(reason: "keychain locked"), lastSync: nil, now: now)
                == ActivityLine(
                    title: "Can't reach the secret store", detail: "keychain locked"))
        #expect(
            p.condition(.offline(nextRetry: now.addingTimeInterval(30)), lastSync: nil, now: now)
                == ActivityLine(title: "Offline", detail: "Next retry in 30 seconds"))
        #expect(
            p.condition(
                .notAnswering(nextRetry: now.addingTimeInterval(90)), lastSync: nil, now: now)
                == ActivityLine(
                    title: "Proton isn't answering", detail: "Next retry in 1 minute"))
        #expect(
            p.condition(
                .rateLimited(until: now.addingTimeInterval(120)), lastSync: nil, now: now)
                == ActivityLine(
                    title: "Proton asked Sweetbriar to slow down",
                    detail: "Until \(time(now.addingTimeInterval(120)))"))
        #expect(
            p.condition(.healthy, lastSync: now.addingTimeInterval(-120), now: now)
                == ActivityLine(title: "Last synced 2 minutes ago"))
        #expect(
            p.condition(.healthy, lastSync: nil, now: now)
                == ActivityLine(title: "Not synced since launch"))
    }

    @Test
    func aBreakerEventCountsMessages() {
        let p = presentation
        #expect(p.breakerEvent(activityBreakerEvent("e", disappeared: 1))
            == "Review 1 disappeared message")
        #expect(p.breakerEvent(activityBreakerEvent("e", disappeared: 12))
            == "Review 12 disappeared messages")
    }

    @Test
    func signInAgainIsOfferedWhereTheSpecSays() {
        #expect(ActivityPresentation.offersSignInAgain(.signInAgain))
        #expect(ActivityPresentation.offersSignInAgain(.disconnected))
        let others: [AccountCondition] = [
            .paused, .verify, .versionRefused(message: "2064"),
            .secretStore(reason: "locked"), .offline(nextRetry: now),
            .notAnswering(nextRetry: now), .rateLimited(until: now), .healthy,
        ]
        for condition in others {
            #expect(!ActivityPresentation.offersSignInAgain(condition))
        }
    }

    @Test
    func intentRowsNameTheKindAndWhyItWaits() {
        let p = presentation
        #expect(
            p.intent(activityIntentRow(state: .parked, lastError: "offline"))
                == ActivityLine(title: "Move", detail: "Parked · offline"))
        #expect(
            p.intent(activityIntentRow(state: .quarantined, reason: .epochChange))
                == ActivityLine(title: "Move", detail: "Quarantined · Server epoch changed"))
        #expect(
            p.intent(activityIntentRow(kind: .deleteToTrash, state: .quarantined, reason: .breaker))
                == ActivityLine(
                    title: "Delete to trash", detail: "Quarantined · Disappeared messages"))
        #expect(
            p.intent(activityIntentRow(kind: .setFlags, state: .parked))
                == ActivityLine(title: "Set flags", detail: "Parked"))
        #expect(
            p.intent(activityIntentRow(kind: .removeLabel, state: .parked))
                == ActivityLine(title: "Remove label", detail: "Parked"))
    }

    @Test
    func recoveryRowsNameTheStateMailboxAndLastSeen() {
        let p = presentation
        let row = activityRecoveryRow(state: .vanished)
        let shown = p.recovery(row, now: row.lastSeenAt.addingTimeInterval(60))
        #expect(shown.title == "Vanished")
        #expect(shown.detail == "Inbox · Last seen 1 minute ago")
        #expect(
            p.recovery(activityRecoveryRow(state: .epochStale), now: now).title
                == "Server epoch changed")
        #expect(
            p.recovery(activityRecoveryRow(state: .superseded), now: now).title == "Superseded")
    }

    /// spec/04-sync-proton "Coverage": the listing names the line, the
    /// counts follow, and a mailbox shows Proton's total once known.
    @Test
    func protonCoverageNamesTheListingAndCounts() {
        let p = ActivityPresentation(locale: Locale(identifier: "en_US"), timeZone: timeZone)
        var coverage = ProtonCoverage(
            listing: .complete, resync: false, mailboxes: [],
            messages: 1_210, withBody: 1_204, bodyFailures: 0
        )
        #expect(
            p.protonCoverage(coverage)
                == ActivityLine(
                    title: "Proton messages synced", detail: "1,210 messages, 1,204 with bodies"))
        coverage.bodyFailures = 2
        #expect(
            p.protonCoverage(coverage).detail
                == "1,210 messages, 1,204 with bodies, 2 couldn't be read")
        coverage.listing = .listing
        #expect(p.protonCoverage(coverage).title == "Listing Proton messages")
        coverage.resync = true
        #expect(p.protonCoverage(coverage).title == "Proton asked Sweetbriar to resync")
        coverage.listing = .none
        coverage.resync = false
        coverage.messages = 1
        coverage.withBody = 0
        coverage.bodyFailures = 0
        #expect(
            p.protonCoverage(coverage)
                == ActivityLine(title: "Listing Proton messages", detail: "1 message, 0 with bodies"))

        #expect(
            p.protonMailbox(
                ProtonMailboxCoverage(mailboxId: 1, name: "Inbox", local: 1_204, remoteTotal: 1_210))
                == ActivityLine(title: "Inbox", detail: "1,204 of 1,210"))
        #expect(
            p.protonMailbox(
                ProtonMailboxCoverage(mailboxId: 2, name: "Travel", local: 3, remoteTotal: nil))
                == ActivityLine(title: "Travel", detail: "3"))
    }
}
