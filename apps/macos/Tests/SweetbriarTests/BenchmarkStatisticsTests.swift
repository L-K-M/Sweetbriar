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
