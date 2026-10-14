// The benchmark's continuously running `CADisplayLink` (plan M0.10):
// one link on the run's window, ticking for the run's whole life —
// separate from the model's pausing `DisplayLinkClock`. Each tick
// samples the main thread's CPU time, the tick's timestamp and the
// process's `phys_footprint`, plus the per-tick screen state the
// wheel events' render-ready resolves from.

import AppKit

/// What the sampler learned at one tick (or at link start).
struct BenchmarkTick {
    /// `CADisplayLink.timestamp` — the same `CACurrentMediaTime`
    /// timebase the events stamp with.
    var timestamp: Double
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
    /// `hasPendingLayout` at the tick.
    var pendingLayout: Bool
    /// The model's generation left the fill's — the abort condition.
    var generationChanged: Bool
}

/// Ticks once per display frame for the run's life. `extras` reads
/// the live screen state each tick supplies; `onTick` runs after the
/// tick lands so the runner can evaluate its pending expectations.
@MainActor
final class BenchmarkTickSampler: NSObject {
    /// What one tick needs from the live list — `nil` before install.
    struct Extras {
        var scrollPosition: Double?
        var visibleRowsPopulated: Bool = false
        var pendingLayout: Bool = true
        var generationChanged: Bool = false
    }

    private var link: CADisplayLink?
    /// The tick log — element 0 is the link-start baseline the first
    /// interval measures from.
    private(set) var ticks: [BenchmarkTick] = []

    var extras: (@MainActor () -> Extras)?
    var onTick: (@MainActor (Int) -> Void)?

    /// Sample once immediately (the baseline), then every frame.
    func start(on view: NSView) {
        ticks = [sample(timestamp: CACurrentMediaTime())]
        let link = view.displayLink(target: self, selector: #selector(tickFired(_:)))
        link.add(to: .main, forMode: .common)
        self.link = link
    }

    /// Stop ticking. Idempotent — the window's death doesn't restart
    /// it.
    func stop() {
        link?.invalidate()
        link = nil
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
            cpuNanoseconds: SystemProbes.threadCPUNanoseconds(),
            footprintBytes: SystemProbes.physicalFootprint(),
            scrollPosition: extras.scrollPosition,
            visibleRowsPopulated: extras.visibleRowsPopulated,
            pendingLayout: extras.pendingLayout,
            generationChanged: extras.generationChanged
        )
    }
}
