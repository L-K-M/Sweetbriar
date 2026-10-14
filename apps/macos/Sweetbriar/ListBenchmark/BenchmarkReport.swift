// The benchmark's report model (plan M0.10): a `Codable` tree encoded
// as pretty JSON with sorted keys. Foundation only, so it type-checks
// on Linux and `SweetbriarTests` drives it.

import Foundation

/// The whole report: `ok` is every check across every run having
/// passed; `failures` gives each reason, prefixed with its run.
struct ListBenchmarkReport: Codable, Equatable {
    var ok: Bool
    var failures: [String]
    var environment: BenchmarkEnvironment
    var runs: [BenchmarkRun]
    var comparison: BenchmarkComparison?

    /// Pretty-printed, sorted-keys JSON (plan M0.10).
    func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(self)
    }

    /// The short stdout summary the launch-argument run prints before
    /// quitting (spec/08 "Debug menu").
    var stdoutSummary: String {
        var lines = ["list-benchmark: ok=\(ok)"]
        for failure in failures {
            lines.append("  failed: \(failure)")
        }
        for run in runs {
            let overall = run.phases.overall
            lines.append(
                "  \(run.implementation) \(run.pass.rawValue): "
                    + "cpu p95 \(format(overall.cpuMilliseconds.p95)) ms, "
                    + "latency p95 \(format(overall.latencyMilliseconds.p95)) ms, "
                    + "hitches \(run.intervals.hitchCount), "
                    + "first rows \(format(run.firstRowsMilliseconds)) ms, "
                    + "ok=\(run.ok)"
            )
        }
        if let comparison {
            lines.append(
                "  comparison meets all bars: \(comparison.meetsAllBars)"
            )
        }
        return lines.joined(separator: "\n")
    }

    private func format(_ value: Double?) -> String {
        value.map { String(format: "%.2f", $0) } ?? "—"
    }
}

/// The machine the run measured (plan M0.10): hardware, OS, Xcode,
/// refresh rate, the list's fixed shape and the pre-run baseline.
struct BenchmarkEnvironment: Codable, Equatable {
    /// `hw.model` — e.g. "Mac14,2".
    var hwModel: String
    /// `machdep.cpu.brand_string`.
    var cpuBrand: String
    /// `kern.hv_vmm_present` — the runner is virtualized.
    var virtualized: Bool
    var osVersion: String
    /// `DTXcode` / `DTXcodeBuild` from the app's Info.plist.
    var xcodeVersion: String
    var xcodeBuild: String
    /// The screen's `maximumFramesPerSecond`, and the nominal interval
    /// derived from it — the hitch boundary's divisor.
    var refreshRateHertz: Double
    var nominalIntervalMilliseconds: Double
    /// N — the fill's row count.
    var rowCount: Int
    var rowPitchPoints: Double
    /// The fixed content size, e.g. "900x640".
    var windowSize: String
    /// `phys_footprint` sampled before the fill — the benchmark-wide
    /// baseline every run's rise reads against.
    var baselineFootprintBytes: UInt64
}

/// One run's report: which implementation, cold or warm, its checks,
/// its per-phase and overall numbers, and the raw samples.
struct BenchmarkRun: Codable, Equatable {
    /// Which pass this is — `cold` is the implementation's first run
    /// in the process, `warm` its second (plan M0.10: ABBA).
    enum Pass: String, Codable {
        case cold
        case warm
    }

    var implementation: String
    var pass: Pass
    /// Every check this run's phases and state passed.
    var ok: Bool
    /// Every check this run failed or timed out, empty when the
    /// run passed.
    var failures: [String]
    /// Whether the window was key and the app active when the run
    /// began — the runs are only comparable when both hold.
    var windowWasKey: Bool
    var appWasActive: Bool
    /// The generation every model must hold for the run's whole life;
    /// `generationChanged` marks the abort.
    var generation: UInt64
    var generationChanged: Bool

    /// Window opening → render-ready of the first visible rows.
    var firstRowsMilliseconds: Double?
    /// The footprint at the run's start (before the window opened).
    var baselineFootprintBytes: UInt64
    var peakFootprintBytes: UInt64
    /// `peak − baselineFootprintBytes` — the rise over the pre-run
    /// baseline taken before this run's window opened.
    var footprintRiseBytes: Int64

    var phases: BenchmarkPhases
    /// The run's display intervals: how many, how many carried work,
    /// how many were hitches.
    var intervals: IntervalSummary
    /// Every posted input and page delivery, with its latency — the
    /// raw samples the percentiles summarize.
    var events: [BenchmarkEvent]
}

/// Per phase (jumps, wheel, selection, page deliveries) and overall.
struct BenchmarkPhases: Codable, Equatable {
    var jumps: PhaseStatistics
    var wheel: PhaseStatistics
    var selection: PhaseStatistics
    var pageDeliveries: PhaseStatistics
    var overall: PhaseStatistics
}

/// One phase's numbers (plan M0.10): CPU per active interval and
/// latency per event, each p50/p95/max over raw samples, plus hitch
/// and event counts.
struct PhaseStatistics: Codable, Equatable {
    var cpuMilliseconds: SampleStatistics
    var latencyMilliseconds: SampleStatistics
    /// Display intervals over 1.5 × the nominal interval, among this
    /// phase's active intervals.
    var hitchCount: Int
    var eventCount: Int
    /// Events whose expected state was never observed on screen —
    /// their latency is capped at the run's end.
    var unresolvedEvents: Int
}

/// p50/p95/max over raw samples (nearest-rank), keeping the samples.
struct SampleStatistics: Codable, Equatable {
    var count: Int
    var p50: Double
    var p95: Double
    var max: Double
    var samples: [Double]
}

/// The run's display-interval ledger.
struct IntervalSummary: Codable, Equatable {
    /// Intervals between consecutive ticks, first to last.
    var count: Int
    /// Intervals an event's span covered — input or delivery to its
    /// render-ready.
    var activeCount: Int
    /// Intervals longer than 1.5 × the nominal one, run-wide.
    var hitchCount: Int
    var nominalMilliseconds: Double
}

/// One posted input or page delivery (plan M0.10): when it was
/// posted, which interval it fell in, which interval its expected
/// state first held on screen, and the latency between them.
struct BenchmarkEvent: Codable, Equatable {
    var phase: String
    var detail: String
    /// `CACurrentMediaTime()` at post — same timebase as the ticks.
    var postedAtSeconds: Double
    /// The tick index ending the interval the post fell in.
    var intervalIndex: Int
    /// The first tick the expected state held on screen; nil when it
    /// never did — `latencyMilliseconds` is then capped at the run's
    /// end.
    var renderReadyIntervalIndex: Int?
    var latencyMilliseconds: Double
}

/// The informational comparison (plan M0.10): per pass, SwiftUI's
/// numbers over AppKit's, plus the decision bars. The owner's Mac
/// decides; CI's numbers are diagnostic.
struct BenchmarkComparison: Codable, Equatable {
    var cold: RunComparison?
    var warm: RunComparison?
    var meetsAllBars: Bool
}

/// One pass's side-by-side numbers and bar results.
struct RunComparison: Codable, Equatable {
    var phases: PhaseComparisons
    var firstRowsMilliseconds: MetricComparison
    var peakFootprintBytes: MetricComparison
    var footprintRiseBytes: MetricComparison
    var bars: [BenchmarkBarResult]
    var allBarsMet: Bool
}

struct PhaseComparisons: Codable, Equatable {
    var jumps: PhaseComparison
    var wheel: PhaseComparison
    var selection: PhaseComparison
    var pageDeliveries: PhaseComparison
    var overall: PhaseComparison
}

/// SwiftUI ÷ AppKit per number (plan M0.10); `within120Percent` is
/// the plan's match bar for CPU and latency p95 and max.
struct PhaseComparison: Codable, Equatable {
    var cpuP50: MetricComparison
    var cpuP95: MetricComparison
    var cpuMax: MetricComparison
    var latencyP50: MetricComparison
    var latencyP95: MetricComparison
    var latencyMax: MetricComparison
    var hitches: MetricComparison
}

/// `swiftui / appkit` — null when AppKit measured zero and SwiftUI
/// didn't (a ratio JSON can't write).
struct MetricComparison: Codable, Equatable {
    var appkit: Double
    var swiftui: Double
    var ratio: Double?
    var within120Percent: Bool
}

/// One decision bar's outcome (plan M0.10): the name, the measured
/// pair or single value, the limit, and whether it held.
struct BenchmarkBarResult: Codable, Equatable {
    var name: String
    var pass: Bool
    var detail: String
    var appkit: Double?
    var swiftui: Double?
    var limit: Double?
}
