// `FrameBatcher`: spec/07 requires results to reach `@MainActor`
// observers "in batches, at most one per frame".

/// Collects events posted from any context and delivers them as one
/// batch on the next display frame.
///
/// - `post(_:)` is `nonisolated` and cheap: it hops to the main actor.
/// - Everything posted between two frames arrives as a single
///   `[Event]` batch; no frame, no delivery; at most one delivery per
///   frame.
/// - No events between frames means no delivery — batches are never
///   empty.
///
/// Being `@MainActor` makes it `Sendable`: every mutable property is
/// main-actor isolated, and the only nonisolated entry point (`post`)
/// crosses to the main actor before touching them.
@MainActor
public final class FrameBatcher<Event: Sendable> {
    private let clock: any FrameClock
    private let deliver: @MainActor ([Event]) -> Void
    private var pending: [Event] = []
    private var frameWaitRunning = false

    /// - Parameters:
    ///   - clock: where frames come from.
    ///   - deliver: called once per frame on the main actor with
    ///     everything posted since the previous frame.
    public init(
        clock: any FrameClock,
        deliver: @escaping @MainActor ([Event]) -> Void
    ) {
        self.clock = clock
        self.deliver = deliver
    }

    /// Post an event from any context. It joins the batch delivered on
    /// the next frame.
    nonisolated public func post(_ event: Event) {
        Task { @MainActor in
            self.enqueue(event)
        }
    }

    private func enqueue(_ event: Event) {
        pending.append(event)
        guard !frameWaitRunning else {
            return
        }
        frameWaitRunning = true
        Task { @MainActor [weak self] in
            await self?.waitForFrameAndDeliver()
        }
    }

    private func waitForFrameAndDeliver() async {
        await clock.nextFrame()
        frameWaitRunning = false
        let batch = pending
        pending.removeAll()
        guard !batch.isEmpty else {
            return
        }
        deliver(batch)
    }
}
