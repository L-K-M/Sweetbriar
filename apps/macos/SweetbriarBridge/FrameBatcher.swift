// `FrameBatcher`: spec/07 requires results to reach `@MainActor`
// observers "in batches, at most one per frame", and a batch posted
// after an idle frame to land at once.

/// Collects events posted from any context and delivers them in
/// batches, at most one per display frame.
///
/// - `post(_:)` is `nonisolated` and cheap: it hops to the main actor.
/// - A post after a frame that delivered nothing lands at once, so an
///   idle list's opening page doesn't wait for the display link to
///   wake. Everything posted after it, until the next frame, arrives
///   on that frame as a single `[Event]` batch.
///
///       post(a) after an idle frame     → [a] at once
///       post(b), post(c) before a tick  → [b, c] on that tick
///       a tick with nothing pending     → idle again
///
/// - No events means no delivery — batches are never empty.
///
/// Being `@MainActor` makes it `Sendable`: every mutable property is
/// main-actor isolated, and the only nonisolated entry point (`post`)
/// crosses to the main actor before touching them.
@MainActor
public final class FrameBatcher<Event: Sendable> {
    private let clock: any FrameClock
    private let deliver: @MainActor ([Event]) -> Void
    private var pending: [Event] = []
    /// A batch landed since the last frame, or the frame wait that
    /// clears this is still running: a post now waits for the frame.
    private var frameBusy = false

    /// - Parameters:
    ///   - clock: where frames come from.
    ///   - deliver: called on the main actor, at most once per frame,
    ///     with everything posted since the previous delivery.
    public init(
        clock: any FrameClock,
        deliver: @escaping @MainActor ([Event]) -> Void
    ) {
        self.clock = clock
        self.deliver = deliver
    }

    /// Post an event from any context. It lands at once after an idle
    /// frame, else with the batch delivered on the next frame.
    nonisolated public func post(_ event: Event) {
        Task { @MainActor in
            self.enqueue(event)
        }
    }

    /// Events waiting for the next frame — for tests.
    var pendingCount: Int { pending.count }

    /// No batch landed since the last frame, so a post lands at once —
    /// for tests.
    var isIdle: Bool { !frameBusy }

    private func enqueue(_ event: Event) {
        pending.append(event)
        guard !frameBusy else {
            return
        }
        frameBusy = true
        deliverPending()
        Task { @MainActor [weak self] in
            await self?.deliverOnFramesUntilIdle()
        }
    }

    /// Each frame delivers what was posted since the last delivery;
    /// the first frame with nothing to deliver ends the busy spell.
    private func deliverOnFramesUntilIdle() async {
        while true {
            await clock.nextFrame()
            guard !pending.isEmpty else {
                break
            }
            deliverPending()
        }
        frameBusy = false
    }

    private func deliverPending() {
        let batch = pending
        pending.removeAll()
        deliver(batch)
    }
}
