// The List Prototype window's plumbing under test (plan M0.10): the
// install tickets that keep a superseded install, or a replaced
// model's open, out of a window it no longer owns; and the
// display-link clock releasing its waiters when its window closes.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
import AppKit
import Synchronization
import Testing

@Suite("install tickets")
struct InstallTicketsTests {
    @Test("only the newest ticket owns the window")
    func newest() {
        var tickets = InstallTickets()
        let first = tickets.next()
        #expect(tickets.isCurrent(first))

        let second = tickets.next()
        #expect(!tickets.isCurrent(first))
        #expect(tickets.isCurrent(second))
    }

    @Test("closing the window makes every ticket stale")
    func close() {
        var tickets = InstallTickets()
        let ticket = tickets.next()
        tickets.close()
        #expect(!tickets.isCurrent(ticket))

        let later = tickets.next()
        #expect(!tickets.isCurrent(later))
    }
}

@Suite("the display-link clock")
@MainActor
struct DisplayLinkClockTests {
    @Test("invalidating it releases parked waits, and later ones don't park")
    func invalidate() async {
        // A view in no window: its link never ticks, so only
        // `invalidate` can end the wait.
        let clock = DisplayLinkClock(view: NSView())
        let parked = Flag()
        Task {
            await clock.nextFrame()
            parked.set()
        }
        try? await Task.sleep(for: .milliseconds(100))
        #expect(!parked.isSet, "a windowless display link ticked")

        clock.invalidate()
        #expect(await within(.seconds(2)) { parked.isSet })

        let late = Flag()
        Task {
            await clock.nextFrame()
            late.set()
        }
        #expect(await within(.seconds(2)) { late.isSet })
    }
}

/// A one-way flag the clock's waiters set from any context.
private final class Flag: Sendable {
    private let state = Mutex(false)

    func set() {
        state.withLock { $0 = true }
    }

    var isSet: Bool {
        state.withLock { $0 }
    }
}

/// Poll `condition` until it holds or `limit` passes — a broken clock
/// fails the test in seconds instead of hanging it.
private func within(
    _ limit: Duration,
    _ condition: @Sendable () -> Bool
) async -> Bool {
    let deadline = ContinuousClock.now + limit
    while !condition() {
        if ContinuousClock.now >= deadline {
            return false
        }
        try? await Task.sleep(for: .milliseconds(10))
    }
    return true
}
