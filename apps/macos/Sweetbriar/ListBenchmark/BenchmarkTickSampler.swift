// The benchmark's continuously running `CADisplayLink` (plan M0.10):
// one link on the run's window, ticking for the run's whole life —
// separate from the model's pausing `DisplayLinkClock`. Each tick
// samples the main thread's CPU time, the tick's timestamp and the
// process's `phys_footprint`, plus the per-tick screen state. A
// main-run-loop observer reports each commit point, where events get
// laid out and committed.

import AppKit

/// What the sampler learned at one tick (or at link start).
struct BenchmarkTick {
    /// `CADisplayLink.timestamp` — the frame's time, in the same
    /// `CACurrentMediaTime` timebase the events stamp with. Interval
    /// lengths come from it.
    var timestamp: Double
    /// `CACurrentMediaTime()` when the tick ran — what commit times
    /// are ordered against.
    var firedAt: Double
    /// `clock_gettime_nsec_np(CLOCK_THREAD_CPUTIME_ID)` — main-thread
    /// CPU, the tick running on the main thread.
    var cpuNanoseconds: UInt64
    /// `task_info(TASK_VM_INFO)` `phys_footprint`.
    var footprintBytes: UInt64
    /// The list's scroll position proxy — `rowRect(forRank: 0).minY`
    /// in window coordinates, which moves with sub-row deltas. Nil
    /// before a list exists.
    var scrollPosition: Double?
    /// Every visible row populated (`model.row(at:)` non-nil for each
    /// `visibleRanks` member) at the tick.
    var visibleRowsPopulated: Bool
    /// The model's generation left the fill's — the abort condition.
    var generationChanged: Bool
}

/// Ticks once per display frame for the run's life. `extras` reads
/// the live screen state each tick supplies; `onTick` runs after the
/// tick lands so the runner can resolve committed events, and
/// `onCommitPoint` runs at each main-run-loop commit point.
@MainActor
final class BenchmarkTickSampler: NSObject {
    /// What one tick needs from the live list — `nil` before install.
    struct Extras {
        var scrollPosition: Double?
        var visibleRowsPopulated: Bool = false
        var generationChanged: Bool = false
    }

    /// After every other before-waiting observer — AppKit's display
    /// cycle and Core Animation's commit (order 2,000,000) included.
    private static let commitObserverOrder = CFIndex.max

    private var link: CADisplayLink?
    private var commitObserver: CFRunLoopObserver?
    /// The tick log — element 0 is the link-start baseline the first
    /// interval is measured from.
    private(set) var ticks: [BenchmarkTick] = []

    var extras: (@MainActor () -> Extras)?
    var onTick: (@MainActor (Int) -> Void)?
    var onCommitPoint: (@MainActor () -> Void)?

    /// Start ticking on `view`'s display, and watching the main run
    /// loop's commit points.
    func start(on view: NSView) {
        ticks = [sample(timestamp: CACurrentMediaTime())]
        let link = view.displayLink(target: self, selector: #selector(tickFired(_:)))
        link.add(to: .main, forMode: .common)
        self.link = link

        // Before waiting, the main run loop has run AppKit's display
        // cycle and committed Core Animation's transaction: what is
        // laid out now goes to the screen with the next frame.
        let observer = CFRunLoopObserverCreateWithHandler(
            kCFAllocatorDefault,
            CFRunLoopActivity.beforeWaiting.rawValue,
            true,
            Self.commitObserverOrder
        ) { [weak self] _, _ in
            MainActor.assumeIsolated {
                self?.onCommitPoint?()
            }
        }
        CFRunLoopAddObserver(CFRunLoopGetMain(), observer, .commonModes)
        commitObserver = observer
    }

    /// Stop the link and the observer.
    func stop() {
        link?.invalidate()
        link = nil
        if let commitObserver {
            CFRunLoopObserverInvalidate(commitObserver)
        }
        commitObserver = nil
    }

    @objc
    private func tickFired(_ link: CADisplayLink) {
        ticks.append(sample(timestamp: link.timestamp))
        onTick?(ticks.count - 1)
    }

    private func sample(timestamp: Double) -> BenchmarkTick {
        let extras = extras?() ?? Extras()
        return BenchmarkTick(
            timestamp: timestamp,
            firedAt: CACurrentMediaTime(),
            cpuNanoseconds: SystemProbes.threadCPUNanoseconds(),
            footprintBytes: SystemProbes.physicalFootprint(),
            scrollPosition: extras.scrollPosition,
            visibleRowsPopulated: extras.visibleRowsPopulated,
            generationChanged: extras.generationChanged
        )
    }
}
