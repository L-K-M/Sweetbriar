// The benchmark's math (plan M0.10): nearest-rank percentiles over
// raw samples, which intervals count as active, the hitch boundary,
// and the informational comparison — SwiftUI ÷ AppKit per
// number plus the decision bars. Foundation only, so it type-checks
// on Linux and `SweetbriarTests` drives it.

import Foundation

/// The run numbers' home — stateless helpers plus the `evaluate`
/// extensions that build the report's comparison.
enum BenchmarkStatistics {
    /// The nearest-rank `p` percentile: rank ⌈p/100 · n⌉ (1-based)
    /// into the sorted samples, clamped to [1, n]. Empty → nil.
    static func percentile(_ samples: [Double], p: Double) -> Double? {
        guard !samples.isEmpty else { return nil }
        let sorted = samples.sorted()
        let rank = Int((p / 100 * Double(sorted.count)).rounded(.up))
        return sorted[min(max(rank, 1), sorted.count) - 1]
    }

    /// p50/p95/max plus the raw samples the report keeps.
    static func summarize(_ samples: [Double]) -> SampleStatistics {
        SampleStatistics(
            count: samples.count,
            p50: percentile(samples, p: 50) ?? 0,
            p95: percentile(samples, p: 95) ?? 0,
            max: samples.max() ?? 0,
            samples: samples
        )
    }

    /// The intervals any event kept active (plan M0.10: an interval is
    /// active from an event's interval through its render-ready
    /// interval). Spans whose render-ready landed before their post —
    /// impossible, but a clamping caller could make one — contribute
    /// nothing.
    static func activeIntervals(_ spans: [EventSpan]) -> Set<Int> {
        var active: Set<Int> = []
        for span in spans where span.renderReadyInterval >= span.eventInterval {
            active.formUnion(span.eventInterval ... span.renderReadyInterval)
        }
        return active
    }

    /// Frame times are vsync times, so an on-time interval equals the
    /// nominal one up to rounding; this much more (in ms) is no hitch.
    static let hitchTimestampNoise = 0.1

    /// A hitch: an interval longer than the display's nominal interval
    /// (plan M0.10), past timestamp rounding — so a 120 Hz frame that
    /// took 8.5 ms counts. Without a nominal interval nothing can be.
    static func isHitch(duration: Double, nominalInterval: Double) -> Bool {
        nominalInterval > 0 && duration > nominalInterval + hitchTimestampNoise
    }
}

/// The span one event keeps its intervals active for (plan M0.10).
/// The run's timeline (plan M0.10): samples are the run's start, one
/// per display tick, and a terminal one at the stop. Interval `i` runs
/// from sample `i − 1` to sample `i`: its CPU is their difference, and
/// a moment belongs to the interval its next sample ends. Only an
/// interval between two display ticks is a frame, with a duration.
enum BenchmarkTimeline {
    /// The interval `time` falls in, given when the samples ran.
    /// Moments past the last sample clamp to the last interval.
    static func intervalIndex(of time: Double, in firedAt: [Double]) -> Int {
        guard firedAt.count > 1 else { return 0 }
        if time <= firedAt[1] { return 1 }
        if time > firedAt[firedAt.count - 1] { return firedAt.count - 1 }
        var lo = 1, hi = firedAt.count - 1
        while lo < hi {
            let mid = (lo + hi) / 2
            if firedAt[mid] >= time {
                hi = mid
            } else {
                lo = mid + 1
            }
        }
        return lo
    }

    /// An event's latency in ms: to the sample it became render-ready
    /// at, or — never ready — a lower bound to the measurement's stop,
    /// which follows every post.
    static func latency(
        postedAt: Double,
        readyAt: Double?,
        stoppedAt: Double
    ) -> (milliseconds: Double, resolved: Bool) {
        (((readyAt ?? stoppedAt) - postedAt) * 1_000, readyAt != nil)
    }

    /// Each interval's frame duration in ms, or nil when it doesn't run
    /// between two display ticks: the first (from the start) and the
    /// terminal one (to the stop) are no frames.
    static func frameDurations(frameTimes: [Double], isFrame: [Bool]) -> [Double?] {
        guard frameTimes.count > 1 else { return [] }
        return (1 ..< frameTimes.count).map { i in
            isFrame[i] && isFrame[i - 1]
                ? (frameTimes[i] - frameTimes[i - 1]) * 1_000
                : nil
        }
    }
}

/// One sample of a run's timeline as the reducer reads it: the
/// sampler's tick without the live list state.
struct TimelineSample: Equatable {
    /// When the sample was taken.
    var firedAt: Double
    /// The frame time a display tick reports; the sample time for the
    /// start and terminal samples.
    var frameTime: Double
    var cpuNanoseconds: UInt64
    var footprintBytes: UInt64
    /// A display tick — only tick-to-tick intervals are frames.
    var isFrame: Bool
    /// The model showed another generation than the fill's.
    var generationChanged: Bool
}

/// One recorded event as the reducer reads it.
struct TimelineEvent: Equatable {
    var phase: String
    var detail: String
    var postedAt: Double
    /// The sample it was render-ready at; nil if it never was.
    var readySample: Int?
}

/// A run's timeline reduced to plan M0.10's numbers.
struct TimelineReduction: Equatable {
    var phases: BenchmarkPhases
    var intervals: IntervalSummary
    var events: [BenchmarkEvent]
    var firstRowsMilliseconds: Double?
    /// Never-rendered events by phase, the opening's included.
    var unresolved: [String: Int]
    var peakFootprintBytes: UInt64
    /// Some sample — the terminal one included — saw another
    /// generation: the run measured a changed source.
    var generationChanged: Bool
}

extension BenchmarkTimeline {
    /// The opening's phase: from the window opening to its first rows.
    static let openingPhase = "firstRows"

    /// The phases with statistics of their own; the opening counts
    /// toward the overall ones.
    static let measuredPhases = ["jumps", "wheel", "selection", "pageDeliveries"]

    /// Reduce a run (plan M0.10): per phase and overall, main-thread
    /// CPU over active intervals, latency over rendered events, and
    /// hitches; first rows; peak footprint. An event's active span
    /// runs from its interval to its render-ready one — or, never
    /// rendered, to where the next event of its phase superseded it or
    /// the run stopped; the opening's always counts, so a stalled
    /// opening's work stays in the numbers. Events posted after
    /// `sealedAt` — measuring ended — aren't the run's. Out-of-range
    /// inputs clamp: a ready sample that doesn't exist is no render.
    static func reduce(
        samples: [TimelineSample],
        events allEvents: [TimelineEvent],
        sealedAt: Double,
        nominalMilliseconds nominalMs: Double
    ) -> TimelineReduction {
        let events = allEvents.filter { $0.postedAt <= sealedAt }
        var cpuMs: [Double] = []
        if samples.count > 1 {
            for i in 1 ..< samples.count {
                cpuMs.append(
                    Double(samples[i].cpuNanoseconds - samples[i - 1].cpuNanoseconds) / 1e6
                )
            }
        }
        let firedAt = samples.map(\.firedAt)
        let durations = frameDurations(
            frameTimes: samples.map(\.frameTime),
            isFrame: samples.map(\.isFrame)
        )
        func cpu(_ interval: Int) -> Double? {
            interval >= 1 && interval <= cpuMs.count ? cpuMs[interval - 1] : nil
        }
        func isHitch(_ interval: Int) -> Bool {
            guard interval >= 1, interval <= durations.count,
                  let duration = durations[interval - 1]
            else {
                return false
            }
            return BenchmarkStatistics.isHitch(duration: duration, nominalInterval: nominalMs)
        }
        // The stop follows every post; with no samples at all, the
        // last post stands in, so no bound goes negative.
        let stoppedAt = firedAt.last ?? events.map(\.postedAt).max() ?? 0

        var reportEvents: [BenchmarkEvent] = []
        var spans: [String: [EventSpan]] = [:]
        var latencies: [String: [Double]] = [:]
        var unresolved: [String: Int] = [:]
        var firstRows: Double?
        for (position, event) in events.enumerated() {
            let interval = intervalIndex(of: event.postedAt, in: firedAt)
            let readySample = event.readySample.flatMap { firedAt.indices.contains($0) ? $0 : nil }
            let readyAt = readySample.map { firedAt[$0] }
            let latency = self.latency(
                postedAt: event.postedAt, readyAt: readyAt, stoppedAt: stoppedAt
            )
            let supersededAt = events[(position + 1)...]
                .first { $0.phase == event.phase }?.postedAt ?? stoppedAt
            let readyInterval = readySample
                ?? intervalIndex(of: supersededAt, in: firedAt)
            reportEvents.append(BenchmarkEvent(
                phase: event.phase,
                detail: event.detail,
                postedAtSeconds: event.postedAt,
                intervalIndex: interval,
                renderReadyIntervalIndex: readySample,
                latencyMilliseconds: latency.milliseconds
            ))
            let bucket = event.phase == openingPhase ? "overall" : event.phase
            spans[bucket, default: []].append(EventSpan(
                eventInterval: interval, renderReadyInterval: readyInterval
            ))
            if !latency.resolved {
                unresolved[event.phase, default: 0] += 1
            } else if event.phase == openingPhase {
                firstRows = latency.milliseconds
            } else {
                latencies[event.phase, default: []].append(latency.milliseconds)
            }
        }

        // Only intervals that exist count as active: a span over a run
        // with no intervals covers none.
        let intervals = cpuMs.isEmpty ? Set<Int>() : Set(1 ... cpuMs.count)
        func phase(_ name: String) -> PhaseStatistics {
            let active = BenchmarkStatistics.activeIntervals(spans[name] ?? [])
                .intersection(intervals)
            return PhaseStatistics(
                cpuMilliseconds: BenchmarkStatistics.summarize(
                    active.sorted().compactMap(cpu)
                ),
                latencyMilliseconds: BenchmarkStatistics.summarize(latencies[name] ?? []),
                hitchCount: active.filter(isHitch).count,
                eventCount: spans[name]?.count ?? 0,
                unresolvedEvents: unresolved[name] ?? 0
            )
        }
        let overallActive = BenchmarkStatistics.activeIntervals(
            measuredPhases.flatMap { spans[$0] ?? [] } + (spans["overall"] ?? [])
        ).intersection(intervals)
        let overall = PhaseStatistics(
            cpuMilliseconds: BenchmarkStatistics.summarize(
                overallActive.sorted().compactMap(cpu)
            ),
            latencyMilliseconds: BenchmarkStatistics.summarize(
                measuredPhases.flatMap { latencies[$0] ?? [] }
            ),
            hitchCount: overallActive.filter(isHitch).count,
            eventCount: reportEvents.count,
            unresolvedEvents: unresolved.values.reduce(0, +)
        )

        return TimelineReduction(
            phases: BenchmarkPhases(
                jumps: phase("jumps"),
                wheel: phase("wheel"),
                selection: phase("selection"),
                pageDeliveries: phase("pageDeliveries"),
                overall: overall
            ),
            intervals: IntervalSummary(
                count: cpuMs.count,
                activeCount: overallActive.count,
                // Run-wide, over every frame: stalls outside event spans
                // count too.
                hitchCount: (1 ... max(1, cpuMs.count)).filter(isHitch).count,
                nominalMilliseconds: nominalMs
            ),
            events: reportEvents,
            firstRowsMilliseconds: firstRows,
            unresolved: unresolved,
            peakFootprintBytes: samples.map(\.footprintBytes).max() ?? 0,
            generationChanged: samples.contains(where: \.generationChanged)
        )
    }
}

struct EventSpan: Equatable {
    /// The tick index ending the interval the event's post fell in.
    var eventInterval: Int
    /// The tick index at which the event's expected state first held
    /// on screen — the run's last interval when it never did.
    var renderReadyInterval: Int
}

/// The implementation strings the report writes — the runner fills
/// them from `PrototypeImplementation`; the comparison matches on
/// them without naming that AppKit-side enum.
enum BenchmarkImplementation {
    static let appkit = "appkit"
    static let swiftui = "swiftui"
}

extension MetricComparison {
    /// `swiftui / appkit`, and whether SwiftUI is within the plan's
    /// 1.2× match bar. A zero AppKit number gives ratio 1 when both
    /// are zero — the identical measurement — and a null ratio (no
    /// finite JSON number) with a failed bar otherwise.
    init(appkit: Double, swiftui: Double) {
        self.appkit = appkit
        self.swiftui = swiftui
        if appkit == 0 {
            ratio = swiftui == 0 ? 1 : nil
            within120Percent = swiftui == 0
        } else {
            ratio = swiftui / appkit
            within120Percent = swiftui <= appkit * 1.2
        }
    }
}

extension PhaseComparison {
    /// SwiftUI ÷ AppKit for each of the phase's numbers.
    static func evaluate(
        appkit: PhaseStatistics,
        swiftui: PhaseStatistics
    ) -> PhaseComparison {
        PhaseComparison(
            cpuP50: MetricComparison(
                appkit: appkit.cpuMilliseconds.p50,
                swiftui: swiftui.cpuMilliseconds.p50
            ),
            cpuP95: MetricComparison(
                appkit: appkit.cpuMilliseconds.p95,
                swiftui: swiftui.cpuMilliseconds.p95
            ),
            cpuMax: MetricComparison(
                appkit: appkit.cpuMilliseconds.max,
                swiftui: swiftui.cpuMilliseconds.max
            ),
            latencyP50: MetricComparison(
                appkit: appkit.latencyMilliseconds.p50,
                swiftui: swiftui.latencyMilliseconds.p50
            ),
            latencyP95: MetricComparison(
                appkit: appkit.latencyMilliseconds.p95,
                swiftui: swiftui.latencyMilliseconds.p95
            ),
            latencyMax: MetricComparison(
                appkit: appkit.latencyMilliseconds.max,
                swiftui: swiftui.latencyMilliseconds.max
            ),
            hitches: MetricComparison(
                appkit: Double(appkit.hitchCount),
                swiftui: Double(swiftui.hitchCount)
            )
        )
    }
}

extension RunComparison {
    /// One pass's side-by-side and its bars (plan M0.10: CPU and
    /// latency p95 and max within 1.2×; the spec/08 budgets at p95 —
    /// scroll-update CPU < 5 ms, selection feedback < 50 ms, first
    /// rows < 100 ms; peak memory within 1.2×; warm wheel hitches no
    /// more than AppKit's).
    static func evaluate(
        appkit: BenchmarkRun,
        swiftui: BenchmarkRun
    ) -> RunComparison {
        let phases = PhaseComparisons(
            jumps: .evaluate(appkit: appkit.phases.jumps, swiftui: swiftui.phases.jumps),
            wheel: .evaluate(appkit: appkit.phases.wheel, swiftui: swiftui.phases.wheel),
            selection: .evaluate(appkit: appkit.phases.selection, swiftui: swiftui.phases.selection),
            pageDeliveries: .evaluate(appkit: appkit.phases.pageDeliveries, swiftui: swiftui.phases.pageDeliveries),
            overall: .evaluate(appkit: appkit.phases.overall, swiftui: swiftui.phases.overall)
        )
        // A run whose first rows never rendered has no first-rows
        // number; the bar compares a bottomless cost.
        let firstRows = MetricComparison(
            appkit: appkit.firstRowsMilliseconds ?? .greatestFiniteMagnitude,
            swiftui: swiftui.firstRowsMilliseconds ?? .greatestFiniteMagnitude
        )
        let peakFootprint = MetricComparison(
            appkit: Double(appkit.peakFootprintBytes),
            swiftui: Double(swiftui.peakFootprintBytes)
        )
        let footprintRise = MetricComparison(
            appkit: Double(appkit.footprintRiseBytes),
            swiftui: Double(swiftui.footprintRiseBytes)
        )

        var bars: [BenchmarkBarResult] = []
        // Numbers count only from complete runs: both passed their
        // checks, rendered their first rows, and every event rendered —
        // an unresolved event's latency is only a lower bound, and a
        // failed run measured something other than the plan's scenario.
        let complete = [appkit, swiftui].allSatisfy { run in
            run.ok && run.windowWasKey && run.appWasActive
                && run.firstRowsMilliseconds != nil
                && run.phases.overall.unresolvedEvents == 0
                && [run.phases.jumps, run.phases.wheel, run.phases.selection,
                    run.phases.pageDeliveries].allSatisfy { $0.unresolvedEvents == 0 }
        }
        bars.append(BenchmarkBarResult(
            name: "runs.complete",
            pass: complete,
            detail: "both runs passed their checks, key and front, and every event rendered"
        ))
        // "CPU and latency p95 and max within 1.2× AppKit's" — every
        // phase, overall included.
        for (name, phase) in [
            ("jumps", phases.jumps),
            ("wheel", phases.wheel),
            ("selection", phases.selection),
            ("pageDeliveries", phases.pageDeliveries),
            ("overall", phases.overall),
        ] {
            for (metricName, metric) in [
                ("cpu.p95", phase.cpuP95),
                ("cpu.max", phase.cpuMax),
                ("latency.p95", phase.latencyP95),
                ("latency.max", phase.latencyMax),
            ] {
                bars.append(BenchmarkBarResult(
                    name: "\(name).\(metricName).within1.2x",
                    pass: metric.within120Percent,
                    detail: "swiftui ≤ 1.2 × appkit",
                    appkit: metric.appkit,
                    swiftui: metric.swiftui,
                    limit: 1.2
                ))
            }
        }
        // spec/08's budgets at p95: scroll updates < 5 ms main-thread
        // CPU (both scroll phases), selection feedback < 50 ms, first
        // populated rows < 100 ms — SwiftUI's own numbers, not ratios.
        for (name, cpu) in [
            ("jumps", swiftui.phases.jumps.cpuMilliseconds.p95),
            ("wheel", swiftui.phases.wheel.cpuMilliseconds.p95),
        ] {
            bars.append(BenchmarkBarResult(
                name: "\(name).cpu.p95<5ms",
                pass: cpu < 5,
                detail: "scroll-update main-thread CPU under 5 ms",
                swiftui: cpu,
                limit: 5
            ))
        }
        let selectionP95 = swiftui.phases.selection.latencyMilliseconds.p95
        bars.append(BenchmarkBarResult(
            name: "selection.latency.p95<50ms",
            pass: selectionP95 < 50,
            detail: "selection feedback under 50 ms",
            swiftui: selectionP95,
            limit: 50
        ))
        let firstMs = swiftui.firstRowsMilliseconds
        bars.append(BenchmarkBarResult(
            name: "firstRows<100ms",
            pass: firstMs != nil && firstMs! < 100,
            detail: "first populated rows within 100 ms",
            swiftui: firstMs,
            limit: 100
        ))
        bars.append(BenchmarkBarResult(
            name: "peakMemory.within1.2x",
            pass: peakFootprint.within120Percent,
            detail: "peak footprint ≤ 1.2 × appkit",
            appkit: peakFootprint.appkit,
            swiftui: peakFootprint.swiftui,
            limit: 1.2
        ))
        // "no more warm-scroll hitches than AppKit" — a warm-pass bar.
        if swiftui.pass == .warm {
            let appkitHitches = Double(appkit.phases.wheel.hitchCount)
            let swiftuiHitches = Double(swiftui.phases.wheel.hitchCount)
            bars.append(BenchmarkBarResult(
                name: "wheel.hitches<=appkit",
                pass: swiftuiHitches <= appkitHitches,
                detail: "warm wheel hitches ≤ appkit",
                appkit: appkitHitches,
                swiftui: swiftuiHitches
            ))
        }

        return RunComparison(
            phases: phases,
            firstRowsMilliseconds: firstRows,
            peakFootprintBytes: peakFootprint,
            footprintRiseBytes: footprintRise,
            bars: bars,
            allBarsMet: bars.allSatisfy(\.pass)
        )
    }
}

extension BenchmarkComparison {
    /// Pair the four runs by implementation and pass (ABBA), evaluate
    /// each side-by-side, and aggregate the bars. A missing pass's
    /// comparison is null and counts as unmet.
    static func evaluate(runs: [BenchmarkRun]) -> BenchmarkComparison {
        func run(_ implementation: String, _ pass: BenchmarkRun.Pass)
            -> BenchmarkRun?
        {
            runs.first { $0.implementation == implementation && $0.pass == pass }
        }
        let cold = run(BenchmarkImplementation.appkit, .cold).flatMap {
            appkit in
            run(BenchmarkImplementation.swiftui, .cold).map {
                RunComparison.evaluate(appkit: appkit, swiftui: $0)
            }
        }
        let warm = run(BenchmarkImplementation.appkit, .warm).flatMap {
            appkit in
            run(BenchmarkImplementation.swiftui, .warm).map {
                RunComparison.evaluate(appkit: appkit, swiftui: $0)
            }
        }
        return BenchmarkComparison(
            cold: cold,
            warm: warm,
            meetsAllBars: cold?.allBarsMet == true && warm?.allBarsMet == true
        )
    }
}
