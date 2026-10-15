// The benchmark's math (plan M0.10): nearest-rank percentiles, the
// active-interval rule and the hitch boundary. Foundation only, so
// the sources and these tests also build in a scratch SwiftPM package
// on Linux; macOS CI runs them in the app-hosted bundle.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
import Foundation
import Testing

@Suite("percentiles")
struct PercentileTests {
    @Test("an empty sample set has no percentile")
    func empty() {
        #expect(BenchmarkStatistics.percentile([], p: 50) == nil)
        #expect(BenchmarkStatistics.percentile([], p: 95) == nil)
    }

    @Test("one sample is every percentile")
    func oneSample() {
        #expect(BenchmarkStatistics.percentile([42], p: 50) == 42)
        #expect(BenchmarkStatistics.percentile([42], p: 95) == 42)
        #expect(BenchmarkStatistics.percentile([42], p: 100) == 42)
    }

    @Test("tied samples resolve by rank, not interpolation")
    func ties() {
        let samples: [Double] = [1, 1, 1, 2, 2, 2]
        // rank ceil(0.5 × 6) = 3 → the last 1.
        #expect(BenchmarkStatistics.percentile(samples, p: 50) == 1)
        // rank ceil(0.95 × 6) = 6 → the last 2.
        #expect(BenchmarkStatistics.percentile(samples, p: 95) == 2)
    }

    @Test("the nearest-rank rule over unsorted samples")
    func unsorted() {
        #expect(BenchmarkStatistics.percentile([3, 1, 2], p: 50) == 2)
        #expect(BenchmarkStatistics.percentile([9, 7, 8], p: 95) == 9)
        // rank 0 clamps to the first — the minimum.
        #expect(BenchmarkStatistics.percentile([5, 4, 6], p: 1) == 4)
    }

    @Test("summarize keeps the raw samples")
    func summarize() {
        let stats = BenchmarkStatistics.summarize([3, 1, 2, 2])
        #expect(stats.count == 4)
        #expect(stats.p50 == 2)
        #expect(stats.max == 3)
        #expect(stats.samples == [3, 1, 2, 2])
    }
}

@Suite("active intervals")
struct ActiveIntervalTests {
    @Test("an event keeps its interval through render-ready active")
    func span() {
        let active = BenchmarkStatistics.activeIntervals([
            EventSpan(eventInterval: 2, renderReadyInterval: 5)
        ])
        #expect(active == [2, 3, 4, 5])
    }

    @Test("overlapping spans union; a reversed span gives nothing")
    func unionAndReversed() {
        let active = BenchmarkStatistics.activeIntervals([
            EventSpan(eventInterval: 0, renderReadyInterval: 2),
            EventSpan(eventInterval: 2, renderReadyInterval: 4),
            EventSpan(eventInterval: 9, renderReadyInterval: 7),
        ])
        #expect(active == [0, 1, 2, 3, 4])
    }

    @Test("no events means no active intervals")
    func empty() {
        #expect(BenchmarkStatistics.activeIntervals([]).isEmpty)
    }
}

@Suite("hitch boundary")
struct HitchTests {
    @Test("any interval longer than nominal is a hitch")
    func longer() {
        // 60 Hz: one missed frame doubles the interval.
        #expect(BenchmarkStatistics.isHitch(duration: 33.3, nominalInterval: 16.7))
        // 120 Hz: an 8.5 ms interval is longer than 8.33 ms.
        #expect(BenchmarkStatistics.isHitch(duration: 8.5, nominalInterval: 8.333))
    }

    @Test("timestamp rounding noise is no hitch")
    func noise() {
        #expect(!BenchmarkStatistics.isHitch(duration: 10, nominalInterval: 10))
        #expect(!BenchmarkStatistics.isHitch(duration: 10.1, nominalInterval: 10))
        #expect(BenchmarkStatistics.isHitch(duration: 10.11, nominalInterval: 10))
        #expect(!BenchmarkStatistics.isHitch(duration: 100, nominalInterval: 0))
    }
}

@Suite("the run's timeline")
struct TimelineTests {
    /// Samples ran at 0 (the start), 10, 20 and 30 (frames), then the
    /// terminal sample at the stop, 100.
    private let firedAt: [Double] = [0, 10, 20, 30, 100]

    @Test("a moment falls in the interval its next sample ends")
    func intervals() {
        #expect(BenchmarkTimeline.intervalIndex(of: 5, in: firedAt) == 1)
        #expect(BenchmarkTimeline.intervalIndex(of: 10, in: firedAt) == 1)
        #expect(BenchmarkTimeline.intervalIndex(of: 10.5, in: firedAt) == 2)
        // Past the last frame, the terminal interval.
        #expect(BenchmarkTimeline.intervalIndex(of: 60, in: firedAt) == 4)
    }

    @Test("a post after the last frame, then a stall: a positive bound to the stop")
    func postAfterLastFrame() {
        let latency = BenchmarkTimeline.latency(
            postedAt: 40, readyAt: nil, stoppedAt: firedAt.last!
        )
        #expect(latency.milliseconds == 60_000)
        #expect(!latency.resolved)
    }

    @Test("a resolved event's latency runs to its render-ready sample")
    func resolved() {
        let latency = BenchmarkTimeline.latency(
            postedAt: 12, readyAt: 20, stoppedAt: firedAt.last!
        )
        #expect(latency.milliseconds == 8_000)
        #expect(latency.resolved)
    }

    @Test("only frame-to-frame intervals have a frame duration")
    func frameDurations() {
        // The start and the terminal sample are no frames.
        let durations = BenchmarkTimeline.frameDurations(
            frameTimes: [0, 10, 20, 30, 100],
            isFrame: [false, true, true, true, false]
        )
        #expect(durations == [nil, 10_000, 10_000, nil])
    }
}

@Suite("reducing a run")
struct ReducerTests {
    /// A sample `cpu` ms of main-thread time into the run; frame times
    /// equal sample times.
    private func sample(
        _ at: Double, cpu: Double, frame: Bool = true, otherGeneration: Bool = false
    ) -> TimelineSample {
        TimelineSample(
            firedAt: at, frameTime: at,
            cpuNanoseconds: UInt64(cpu * 1e6), footprintBytes: UInt64(at * 10),
            isFrame: frame, generationChanged: otherGeneration
        )
    }

    @Test("an opening that never renders keeps its work in the numbers")
    func stalledOpening() {
        // The start, one tick, then a 100 s stall to the stop.
        let samples = [
            sample(0, cpu: 0, frame: false),
            sample(0.016, cpu: 1),
            sample(100, cpu: 100_001, frame: false),
        ]
        let events = [
            TimelineEvent(phase: "firstRows", detail: "opened", postedAt: 0.001, readySample: nil),
        ]
        let reduction = BenchmarkTimeline.reduce(
            samples: samples, events: events, sealedAt: .infinity, nominalMilliseconds: 16.7
        )
        #expect(reduction.firstRowsMilliseconds == nil)
        #expect(reduction.unresolved == ["firstRows": 1])
        #expect(reduction.phases.overall.unresolvedEvents == 1)
        #expect(reduction.phases.overall.cpuMilliseconds.max == 100_000)
        #expect(reduction.events.first?.latencyMilliseconds == 99_999)
    }

    @Test("a rendered run: latency, active CPU and a missed frame")
    func rendered() {
        // Ticks at 16.7 ms; the frame ending sample 3 is late (33.4 ms).
        let samples = [
            sample(0, cpu: 0, frame: false),
            sample(0.0167, cpu: 2),
            sample(0.0334, cpu: 4),
            sample(0.0668, cpu: 30),
            sample(0.0835, cpu: 31),
            sample(0.09, cpu: 31, frame: false),
        ]
        let events = [
            TimelineEvent(phase: "firstRows", detail: "opened", postedAt: 0.0001, readySample: 1),
            TimelineEvent(phase: "jumps", detail: "jump", postedAt: 0.02, readySample: 3),
        ]
        let reduction = BenchmarkTimeline.reduce(
            samples: samples, events: events, sealedAt: .infinity, nominalMilliseconds: 16.7
        )
        #expect(abs((reduction.firstRowsMilliseconds ?? 0) - 16.6) < 1e-6)
        let jumps = reduction.phases.jumps
        #expect(abs(jumps.latencyMilliseconds.max - 46.8) < 1e-6)
        // The jump's span: intervals 2 and 3, CPU 2 and 26 ms.
        #expect(jumps.cpuMilliseconds.samples == [2, 26])
        #expect(jumps.hitchCount == 1)
        #expect(reduction.unresolved.isEmpty)
        // Run-wide: the late frame only; the terminal interval is none.
        #expect(reduction.intervals.hitchCount == 1)
        #expect(reduction.intervals.count == 5)
    }

    @Test("a post after the last frame resolves to a positive bound")
    func postAfterLastFrame() {
        let samples = [
            sample(0, cpu: 0, frame: false),
            sample(0.0167, cpu: 1),
            sample(5, cpu: 4_000, frame: false),
        ]
        let events = [
            TimelineEvent(phase: "selection", detail: "click", postedAt: 1, readySample: nil),
        ]
        let reduction = BenchmarkTimeline.reduce(
            samples: samples, events: events, sealedAt: .infinity, nominalMilliseconds: 16.7
        )
        #expect(reduction.events.first?.latencyMilliseconds == 4_000)
        #expect(reduction.unresolved == ["selection": 1])
        #expect(reduction.phases.selection.latencyMilliseconds.count == 0)
        #expect(reduction.phases.selection.cpuMilliseconds.samples == [3_999])
    }

    @Test("another generation seen only at the stop still counts")
    func generationAtTheStop() {
        let samples = [
            sample(0, cpu: 0, frame: false),
            sample(0.0167, cpu: 1),
            sample(0.5, cpu: 2, frame: false, otherGeneration: true),
        ]
        let reduction = BenchmarkTimeline.reduce(
            samples: samples, events: [], sealedAt: .infinity, nominalMilliseconds: 16.7
        )
        #expect(reduction.generationChanged)
    }

    @Test("events recorded after the seal aren't the run's")
    func sealed() {
        let samples = [
            sample(0, cpu: 0, frame: false),
            sample(0.0167, cpu: 1),
            sample(0.5, cpu: 2, frame: false),
        ]
        let events = [
            TimelineEvent(phase: "jumps", detail: "jump", postedAt: 0.01, readySample: 1),
            TimelineEvent(phase: "pageDeliveries", detail: "late", postedAt: 0.45, readySample: nil),
        ]
        let reduction = BenchmarkTimeline.reduce(
            samples: samples, events: events, sealedAt: 0.4, nominalMilliseconds: 16.7
        )
        #expect(reduction.events.map(\.detail) == ["jump"])
        #expect(reduction.unresolved.isEmpty)
    }

    @Test("out-of-range inputs clamp instead of inventing numbers")
    func clamps() {
        let samples = [
            sample(0, cpu: 0, frame: false),
            sample(0.0167, cpu: 1, frame: false),
        ]
        let events = [
            TimelineEvent(phase: "jumps", detail: "jump", postedAt: 0.01, readySample: 99),
            TimelineEvent(phase: "jumps", detail: "jump", postedAt: 0.012, readySample: -1),
        ]
        let reduction = BenchmarkTimeline.reduce(
            samples: samples, events: events, sealedAt: .infinity, nominalMilliseconds: 16.7
        )
        #expect(reduction.events.allSatisfy { $0.renderReadyIntervalIndex == nil })
        #expect(reduction.unresolved == ["jumps": 2])
        #expect(reduction.intervals.activeCount <= reduction.intervals.count)

        let empty = BenchmarkTimeline.reduce(
            samples: [], events: events, sealedAt: .infinity, nominalMilliseconds: 16.7
        )
        #expect((empty.events.first?.latencyMilliseconds ?? -1) >= 0)
        #expect(empty.intervals.activeCount <= empty.intervals.count)
    }
}

