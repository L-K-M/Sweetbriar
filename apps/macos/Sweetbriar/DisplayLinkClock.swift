// The real `FrameClock` (spec/07): one `nextFrame()` wait resolves per
// `CADisplayLink` tick of the window's screen. AppKit lives in this
// target only — the bridge just knows the protocol.

import AppKit
import SweetbriarBridge

/// A display link parked on the main run loop. `nextFrame()` waiters
/// all resume on the same tick. The link runs only while someone
/// waits: an idle app doesn't wake every frame.
///
/// `@unchecked Sendable` because the waiters list is behind a lock and
/// the link itself is touched only on the main thread (init, tick, the
/// unpause hop and `deinit`'s invalidate — the app keeps the clock
/// alive for the process's lifetime anyway).
final class DisplayLinkClock: NSObject, FrameClock, @unchecked Sendable {
    private var link: CADisplayLink?
    private let waiters = WaiterList()

    @MainActor
    init(view: NSView) {
        super.init()
        let link = view.displayLink(target: self, selector: #selector(frameDidFire(_:)))
        link.isPaused = true
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    /// Resume everyone waiting, then pause until the next wait. A wait
    /// added after the pause check unpauses through its own main-queue
    /// hop, which runs after this tick: no wait is stranded.
    @objc
    private func frameDidFire(_ link: CADisplayLink) {
        for waiter in waiters.takeAll() {
            waiter.resume()
        }
        if waiters.isEmpty {
            link.isPaused = true
        }
    }

    func nextFrame() async {
        await withCheckedContinuation { waiter in
            // An invalidated clock never ticks again: don't park.
            guard waiters.append(waiter) else {
                waiter.resume()
                return
            }
            DispatchQueue.main.async {
                self.link?.isPaused = false
            }
        }
    }

    /// Stop the link and drop it, breaking its target cycle — a
    /// `CADisplayLink` retains its target, so without this a clock
    /// owned by a closing window would leak the pair. Idempotent.
    /// The waiters parked then resume at once, and later waits don't
    /// park: a stranded wait would keep its frame batcher — and the
    /// pages queued in it — alive forever.
    @MainActor
    func invalidate() {
        link?.invalidate()
        link = nil
        for waiter in waiters.invalidate() {
            waiter.resume()
        }
    }

    deinit {
        link?.invalidate()
    }
}

/// The continuation list shared between the display-link tick (main
/// thread) and `nextFrame()` callers (any context).
private final class WaiterList: @unchecked Sendable {
    private let lock = NSLock()
    private var continuations: [CheckedContinuation<Void, Never>] = []
    private var invalidated = false

    /// Park `continuation` for the next tick — false, parking nothing,
    /// once the clock is invalidated.
    func append(_ continuation: CheckedContinuation<Void, Never>) -> Bool {
        lock.withLock {
            guard !invalidated else { return false }
            continuations.append(continuation)
            return true
        }
    }

    /// Refuse further waits and hand back the parked ones.
    func invalidate() -> [CheckedContinuation<Void, Never>] {
        lock.withLock {
            invalidated = true
            let all = continuations
            continuations.removeAll()
            return all
        }
    }

    var isEmpty: Bool {
        lock.withLock {
            continuations.isEmpty
        }
    }

    func takeAll() -> [CheckedContinuation<Void, Never>] {
        lock.withLock {
            let all = continuations
            continuations.removeAll()
            return all
        }
    }
}
