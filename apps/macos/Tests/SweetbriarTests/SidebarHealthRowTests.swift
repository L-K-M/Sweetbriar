// The sidebar's account health row (plan M2.7d2, spec/08 "Activity /
// account health"): a healthy account with no open events shows no
// row; a non-healthy condition shows its title; open events show one
// summed "Review N disappeared messages" line.

import Foundation
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@Suite("Sidebar health row")
@MainActor
struct SidebarHealthRowTests {
    private let presentation = ActivityPresentation(
        locale: Locale(identifier: "en_US"),
        timeZone: TimeZone(identifier: "UTC")!)
    private let now = Date(timeIntervalSince1970: 1_000)

    @Test func healthyWithoutEventsShowsNoRow() {
        let text = SidebarViewController.healthRowText(
            condition: .healthy, breakerEvents: [],
            presentation: presentation, now: now)
        #expect(text == nil)
    }

    @Test func aNonHealthyConditionShowsItsTitle() {
        let text = SidebarViewController.healthRowText(
            condition: .disconnected, breakerEvents: [],
            presentation: presentation, now: now)
        #expect(text == "Disconnected")
    }

    @Test func openEventsSumTheirDisappearedMessages() {
        let events = [
            AccountBreakerEvent(eventId: "a", mailboxId: nil, disappeared: 1, detectedAt: now),
            AccountBreakerEvent(eventId: "b", mailboxId: nil, disappeared: 4, detectedAt: now),
        ]
        let text = SidebarViewController.healthRowText(
            condition: .healthy, breakerEvents: events,
            presentation: presentation, now: now)
        #expect(text == "Review 5 disappeared messages")
    }

    @Test func aSingleDisappearedMessageUsesTheSingular() {
        let events = [
            AccountBreakerEvent(eventId: "a", mailboxId: nil, disappeared: 1, detectedAt: now),
        ]
        let text = SidebarViewController.healthRowText(
            condition: .healthy, breakerEvents: events,
            presentation: presentation, now: now)
        #expect(text == "Review 1 disappeared message")
    }
}
