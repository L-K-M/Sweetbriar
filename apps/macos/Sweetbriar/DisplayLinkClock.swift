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
            waiters.append(waiter)
            DispatchQueue.main.async {
                self.link?.isPaused = false
            }
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

    func append(_ continuation: CheckedContinuation<Void, Never>) {
        lock.withLock {
            continuations.append(continuation)
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
