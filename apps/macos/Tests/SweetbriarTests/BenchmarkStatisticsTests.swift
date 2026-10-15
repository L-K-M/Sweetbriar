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
    @Test("an interval longer than nominal, past jitter, is a hitch")
    func longer() {
        // 60 Hz: one missed frame doubles the interval.
        #expect(BenchmarkStatistics.isHitch(duration: 33.3, nominalInterval: 16.7))
        // 120 Hz: a 12 ms interval is longer than 8.33 ms.
        #expect(BenchmarkStatistics.isHitch(duration: 12, nominalInterval: 8.33))
    }

    @Test("timestamp jitter within 5% of nominal is not a hitch")
    func jitter() {
        #expect(!BenchmarkStatistics.isHitch(duration: 10, nominalInterval: 10))
        #expect(!BenchmarkStatistics.isHitch(duration: 10.5, nominalInterval: 10))
        #expect(BenchmarkStatistics.isHitch(duration: 10.501, nominalInterval: 10))
        #expect(!BenchmarkStatistics.isHitch(duration: 100, nominalInterval: 0))
    }
}
