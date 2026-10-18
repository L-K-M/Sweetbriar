// The report's shape and the comparison's decision bars (plan
// M0.10): the JSON keys CI's jq reads, the 1.2x match bars, the
// spec/08 budgets, and the warm-wheel hitches bar. Foundation only.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
import Foundation
import Testing

/// A run filled with enough numbers to drive the comparison — every
/// CPU and latency sample `milliseconds`, one event per phase, no
/// hitches, first rows at 50 ms.
private func makeRun(
    implementation: String,
    pass: BenchmarkRun.Pass,
    milliseconds: Double = 1,
    firstRows: Double? = 50,
    wheelHitches: Int = 0,
    ok: Bool = true,
    unresolvedWheelEvents: Int = 0,
    foreground: Bool = true
) -> BenchmarkRun {
    func samples(_ value: Double) -> SampleStatistics {
        SampleStatistics(
            count: 1, p50: value, p95: value, max: value,
            samples: [value]
        )
    }
    func phase(hitches: Int = 0, unresolved: Int = 0) -> PhaseStatistics {
        PhaseStatistics(
            cpuMilliseconds: samples(milliseconds),
            latencyMilliseconds: samples(milliseconds),
            hitchCount: hitches,
            eventCount: 1 + unresolved,
            unresolvedEvents: unresolved
        )
    }
    return BenchmarkRun(
        implementation: implementation,
        pass: pass,
        ok: ok,
        failures: ok ? [] : ["a check failed"],
        windowWasKey: foreground,
        appWasActive: foreground,
        generation: 7,
        generationChanged: false,
        firstRowsMilliseconds: firstRows,
        baselineFootprintBytes: 100,
        peakFootprintBytes: 200,
        footprintRiseBytes: 100,
        phases: BenchmarkPhases(
            jumps: phase(),
            wheel: phase(hitches: wheelHitches, unresolved: unresolvedWheelEvents),
            selection: phase(),
            pageDeliveries: phase(),
            overall: phase()
        ),
        intervals: IntervalSummary(
            count: 100, activeCount: 10, hitchCount: 0,
            nominalMilliseconds: 16.7
        ),
        events: [
            BenchmarkEvent(
                phase: "jumps", detail: "test",
                postedAtSeconds: 1, intervalIndex: 1,
                renderReadyIntervalIndex: 2, latencyMilliseconds: milliseconds
            )
        ]
    )
}

@Suite("the report's JSON")
struct ReportJSONTests {
    @Test("the top-level keys CI's jq reads are present")
    func keys() throws {
        let report = ListBenchmarkReport(
            ok: false,
            failures: ["a reason"],
            environment: BenchmarkEnvironment(
                hwModel: "MacTest", cpuBrand: "CPU", virtualized: false,
                osVersion: "26.0", xcodeVersion: "2600", xcodeBuild: "17A",
                refreshRateHertz: 60, nominalIntervalMilliseconds: 16.7,
                rowCount: 500_000, rowPitchPoints: 56,
                windowSize: "900x640", baselineFootprintBytes: 100
            ),
            runs: [makeRun(implementation: "appkit", pass: .cold)],
            comparison: nil
        )
        let object = try JSONSerialization.jsonObject(
            with: report.encoded()
        ) as? [String: Any]
        let top = try #require(object)
        #expect(top["ok"] as? Bool == false)
        #expect(top["mode"] as? String == "synthetic")
        #expect(report.stdoutSummary.hasPrefix("list-benchmark:"))
        var legacy = top
        legacy.removeValue(forKey: "mode")
        let decodedLegacy = try JSONDecoder().decode(
            ListBenchmarkReport.self, from: JSONSerialization.data(withJSONObject: legacy)
        )
        #expect(decodedLegacy.mode == .synthetic)
        #expect(decodedLegacy.runs == report.runs)
        #expect(top["failures"] as? [String] == ["a reason"])
        #expect(top["environment"] is [String: Any])
        #expect(top["runs"] is [[String: Any]])
        // `comparison` is optional — nil encodes as no key at all.
        #expect(!top.keys.contains("comparison"))
        let withComparison = try JSONSerialization.jsonObject(
            with: ListBenchmarkReport(
                ok: true, failures: [],
                environment: report.environment,
                runs: report.runs,
                comparison: BenchmarkComparison(
                    cold: nil, warm: nil, meetsAllBars: false
                )
            ).encoded()
        ) as? [String: Any]
        #expect((withComparison?["comparison"] as? [String: Any])?["meetsAllBars"] as? Bool == false)
        let environment = try #require(top["environment"] as? [String: Any])
        #expect(environment["hwModel"] as? String == "MacTest")
        #expect(environment["virtualized"] as? Bool == false)
        let run = try #require((top["runs"] as? [[String: Any]])?.first)
        #expect(run["implementation"] as? String == "appkit")
        #expect(run["pass"] as? String == "cold")
        #expect(run["phases"] is [String: Any])
        #expect(run["events"] is [[String: Any]])
    }
}

@Suite("the comparison's bars")
struct ComparisonBarTests {
    @Test("SwiftUI within 1.2x meets the match bars")
    func within() {
        let appkit = makeRun(implementation: "appkit", pass: .cold, milliseconds: 1)
        let swiftui = makeRun(implementation: "swiftui", pass: .cold, milliseconds: 1.2)
        let comparison = RunComparison.evaluate(appkit: appkit, swiftui: swiftui)
        #expect(comparison.allBarsMet)
        let cpuBar = comparison.bars.first { $0.name == "overall.cpu.p95.within1.2x" }
        #expect(cpuBar?.pass == true)
    }

    @Test("a far-slower SwiftUI fails its bars selectively")
    func slower() {
        let appkit = makeRun(implementation: "appkit", pass: .cold, milliseconds: 1)
        // 6 ms everywhere: the 1.2x bars and the < 5 ms CPU budgets
        // fail; selection feedback under 50 ms, first rows at 50 ms
        // and memory still pass.
        let swiftui = makeRun(implementation: "swiftui", pass: .cold, milliseconds: 6)
        let comparison = RunComparison.evaluate(appkit: appkit, swiftui: swiftui)
        #expect(!comparison.allBarsMet)
        #expect(comparison.bars.first { $0.name == "overall.cpu.p95.within1.2x" }?.pass == false)
        #expect(comparison.bars.first { $0.name == "jumps.cpu.p95<5ms" }?.pass == false)
        #expect(comparison.bars.first { $0.name == "selection.latency.p95<50ms" }?.pass == true)
        #expect(comparison.bars.first { $0.name == "firstRows<100ms" }?.pass == true)
        #expect(comparison.bars.first { $0.name == "peakMemory.within1.2x" }?.pass == true)
    }

    @Test("a zero AppKit number: equal is 1, unequal is no ratio")
    func zeroBaseline() {
        let equal = MetricComparison(appkit: 0, swiftui: 0)
        #expect(equal.ratio == 1)
        #expect(equal.within120Percent)
        let unequal = MetricComparison(appkit: 0, swiftui: 5)
        #expect(unequal.ratio == nil)
        #expect(!unequal.within120Percent)
    }

    @Test("the warm-wheel hitches bar only exists on the warm pass")
    func warmWheelBar() {
        let appkitCold = makeRun(implementation: "appkit", pass: .cold)
        let swiftuiCold = makeRun(implementation: "swiftui", pass: .cold)
        let cold = RunComparison.evaluate(appkit: appkitCold, swiftui: swiftuiCold)
        #expect(!cold.bars.contains { $0.name == "wheel.hitches<=appkit" })

        let appkitWarm = makeRun(implementation: "appkit", pass: .warm, wheelHitches: 2)
        let swiftuiWarm = makeRun(implementation: "swiftui", pass: .warm, wheelHitches: 3)
        let warm = RunComparison.evaluate(appkit: appkitWarm, swiftui: swiftuiWarm)
        #expect(warm.bars.first { $0.name == "wheel.hitches<=appkit" }?.pass == false)
    }

    @Test("four ABBA runs pair by pass; all bars met aggregates")
    func pairing() {
        let runs = [
            makeRun(implementation: "appkit", pass: .cold),
            makeRun(implementation: "swiftui", pass: .cold),
            makeRun(implementation: "swiftui", pass: .warm),
            makeRun(implementation: "appkit", pass: .warm),
        ]
        let comparison = BenchmarkComparison.evaluate(runs: runs)
        #expect(comparison.cold != nil)
        #expect(comparison.warm != nil)
        #expect(comparison.meetsAllBars)
    }

    @Test("a missing pass leaves its comparison null and unmet")
    func missing() {
        let comparison = BenchmarkComparison.evaluate(runs: [
            makeRun(implementation: "appkit", pass: .cold)
        ])
        #expect(comparison.cold == nil)
        #expect(comparison.warm == nil)
        #expect(!comparison.meetsAllBars)
    }
}

@Suite("a comparison counts only complete runs")
struct CompleteRunsTests {
    @Test("a SwiftUI run that failed a check meets no bars")
    func failedRun() {
        let comparison = RunComparison.evaluate(
            appkit: makeRun(implementation: "appkit", pass: .cold),
            swiftui: makeRun(implementation: "swiftui", pass: .cold, ok: false)
        )
        #expect(!comparison.allBarsMet)
        #expect(comparison.bars.first { $0.name == "runs.complete" }?.pass == false)
    }

    @Test("unresolved events leave the comparison incomplete")
    func unresolved() {
        let comparison = RunComparison.evaluate(
            appkit: makeRun(implementation: "appkit", pass: .cold),
            swiftui: makeRun(
                implementation: "swiftui", pass: .cold,
                unresolvedWheelEvents: 200
            )
        )
        #expect(!comparison.allBarsMet)
    }

    @Test("a run without first rows leaves it incomplete")
    func noFirstRows() {
        let comparison = RunComparison.evaluate(
            appkit: makeRun(implementation: "appkit", pass: .cold, firstRows: nil),
            swiftui: makeRun(implementation: "swiftui", pass: .cold)
        )
        #expect(comparison.bars.first { $0.name == "runs.complete" }?.pass == false)
    }

    @Test("two complete runs pass the completeness bar")
    func complete() {
        let comparison = RunComparison.evaluate(
            appkit: makeRun(implementation: "appkit", pass: .cold),
            swiftui: makeRun(implementation: "swiftui", pass: .cold)
        )
        #expect(comparison.bars.first { $0.name == "runs.complete" }?.pass == true)
    }

    @Test("a run whose window wasn't key and front leaves it incomplete")
    func background() {
        let comparison = RunComparison.evaluate(
            appkit: makeRun(implementation: "appkit", pass: .cold),
            swiftui: makeRun(implementation: "swiftui", pass: .cold, foreground: false)
        )
        #expect(comparison.bars.first { $0.name == "runs.complete" }?.pass == false)
    }
}

@Suite("store benchmark planning and reporting")
struct StoreBenchmarkTests {
    /// The second entry when only one mailbox holds messages.
    private static let fallback = "fallback"

    @Test("the two largest nonempty selectable candidates win, ties use entry id")
    func pair() {
        let pair = StoreBenchmarkPlan.switchPair([
            .init(entryID: "empty", count: 0),
            .init(entryID: "small", count: 1),
            .init(entryID: "b", count: 100),
            .init(entryID: "a", count: 100),
        ], fallback: Self.fallback)
        #expect(pair == ["a", "b"])
        #expect(StoreBenchmarkPlan.switchPair([], fallback: Self.fallback).isEmpty)
        #expect(StoreBenchmarkPlan.switchPair(
            [.init(entryID: "empty", count: 0)], fallback: Self.fallback
        ).isEmpty)
        #expect(StoreBenchmarkPlan.switchPair(
            [.init(entryID: "only", count: 1)], fallback: Self.fallback
        ) == ["only", Self.fallback])
        #expect(StoreBenchmarkPlan.switchCount == 30)
    }

    @Test("selection stays in range and preserves the synthetic sequence when large enough",
          arguments: [0, 1, 9, 10, 199, 9_999, 10_000, 109_999, 110_000, 500_000])
    func endpoints(count: Int) {
        let range = StoreBenchmarkPlan.selection(count: count)
        guard count > 0 else {
            #expect(range == nil)
            return
        }
        #expect(range?.count == min(count, 10_000))
        #expect((range?.lowerBound ?? -1) >= 0)
        #expect((range?.upperBound ?? count) < count)
        if count >= 110_000 { #expect(range == 100_000 ... 109_999) }
        if count <= 10_000 { #expect(range == 0 ... count - 1) }
    }

    @Test("budget failure leaves functional success alone, insufficient coverage has no verdict")
    func budgets() {
        var run = makeRun(implementation: "appkit", pass: .cold, milliseconds: 6)
        run.events += [
            BenchmarkEvent(phase: "selection", detail: "click rank 0", postedAtSeconds: 1,
                           intervalIndex: 1, renderReadyIntervalIndex: 2, latencyMilliseconds: 6),
            BenchmarkEvent(phase: "selection", detail: "position jump to rank 0", postedAtSeconds: 1,
                           intervalIndex: 1, renderReadyIntervalIndex: 2, latencyMilliseconds: 500),
        ]
        let firstRows = BenchmarkStatistics.summarize([101, 120])
        let covered = StoreBenchmarkPlan.budgets(
            run: run, firstRows: firstRows, rowCount: 500_000,
            selectionSpan: 10_000, scrollSpan: 500_000
        )
        #expect(run.ok)
        #expect(covered.map(\.met) == [false, false, false, true])
        let small = StoreBenchmarkPlan.budgets(
            run: run, firstRows: firstRows, rowCount: 99,
            selectionSpan: 99, scrollSpan: 99
        )
        #expect(small.map(\.met) == [false, nil, nil, nil])
        let invalid = StoreBenchmarkPlan.budgets(
            run: makeRun(implementation: "appkit", pass: .cold, ok: false),
            firstRows: firstRows, rowCount: 500_000,
            selectionSpan: 10_000, scrollSpan: 500_000
        )
        #expect(invalid.allSatisfy { $0.met == nil })
        let empty = StoreBenchmarkPlan.budgets(
            run: run, firstRows: BenchmarkStatistics.summarize([]), rowCount: 0,
            selectionSpan: 0, scrollSpan: 0
        )
        #expect(empty.allSatisfy { $0.met == nil })
    }

    @Test("store schema round-trips with raw switch samples and per-run display")
    func storeJSON() throws {
        var run = makeRun(implementation: "appkit", pass: .cold)
        run.display = BenchmarkDisplay(id: 42, refreshRateHertz: 120, changed: false)
        let firstRows = BenchmarkStatistics.summarize([10, 20, 30])
        let report = ListBenchmarkReport(
            ok: true, failures: [],
            environment: BenchmarkEnvironment(
                hwModel: "MacTest", cpuBrand: "CPU", virtualized: false,
                osVersion: "26.0", xcodeVersion: "2600", xcodeBuild: "17A",
                refreshRateHertz: 120, nominalIntervalMilliseconds: 1_000 / 120,
                rowCount: 12, rowPitchPoints: 56, windowSize: "1100x700",
                baselineFootprintBytes: 100
            ), runs: [run], comparison: nil, mode: .store,
            store: StoreBenchmarkResult(
                switchEntryIDs: ["a/1", "unified/inbox"], completedSwitches: 30,
                firstRows: firstRows, rowCount: 12, selectionAnchor: 0, selectionEnd: 11,
                selectionSpanReached: 12, scrollSpanReached: 12, budgets: []
            )
        )
        let decoded = try JSONDecoder().decode(ListBenchmarkReport.self, from: report.encoded())
        #expect(decoded == report)
        #expect(decoded.mode == .store)
        #expect(decoded.store?.firstRows.samples == [10, 20, 30])
        #expect(decoded.runs.first?.display?.id == 42)
        #expect(decoded.stdoutSummary.hasPrefix("store-benchmark: ok=true"))
    }
}

@Suite("store coverage")
struct StoreCoverageTests {
    @Test("only rendered jumps and a rendered range selection establish coverage")
    func renderedCoverage() {
        var run = makeRun(implementation: "appkit", pass: .cold)
        func event(_ phase: String, _ detail: String, rendered: Bool = true) -> BenchmarkEvent {
            BenchmarkEvent(
                phase: phase, detail: detail, postedAtSeconds: 1, intervalIndex: 1,
                renderReadyIntervalIndex: rendered ? 2 : nil, latencyMilliseconds: 10
            )
        }
        run.events = [
            event("jumps", "jump to rank 0"), event("jumps", "jump to rank 12"),
            event("jumps", "jump to rank 499999", rendered: false),
            event("selection", "shift-click rank 12"),
        ]
        let result = StoreBenchmarkResult.evaluate(
            run: run, pair: ["a", "b"], completedSwitches: 30,
            firstRows: BenchmarkStatistics.summarize([10, 20]), rowCount: 13
        )
        #expect(result.scrollSpanReached == 13)
        #expect(result.selectionSpanReached == 13)
        #expect(result.selectionAnchor == 0)
        #expect(result.selectionEnd == 12)
        #expect(result.budgets.map(\.met) == [true, nil, nil, nil])
        run.events = [event("selection", "shift-click rank 12", rendered: false)]
        let incomplete = StoreBenchmarkResult.evaluate(
            run: run, pair: ["a", "b"], completedSwitches: 0,
            firstRows: BenchmarkStatistics.summarize([]), rowCount: 13
        )
        #expect(incomplete.scrollSpanReached == 0)
        #expect(incomplete.selectionSpanReached == 0)
        #expect(incomplete.firstRows.count == 0)
        #expect(incomplete.budgets.allSatisfy { $0.met == nil })
    }
}
