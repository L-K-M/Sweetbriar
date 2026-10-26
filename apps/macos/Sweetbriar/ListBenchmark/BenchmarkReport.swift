// The benchmark's report model (plan M0.10): a `Codable` tree encoded
// as pretty JSON with sorted keys. Foundation only, so it type-checks
// on Linux and `SweetbriarTests` drives it.

import Foundation

/// The whole report: `ok` covers functional checks across every run;
/// budget verdicts live separately in `comparison`, `store` or `search`.
struct ListBenchmarkReport: Codable, Equatable {
    var ok: Bool
    var failures: [String]
    var environment: BenchmarkEnvironment
    var runs: [BenchmarkRun]
    var comparison: BenchmarkComparison?
    var mode: BenchmarkMode = .synthetic
    var store: StoreBenchmarkResult?
    var search: SearchBenchmarkResult?

    /// Pretty-printed, sorted-keys JSON (plan M0.10).
    func encoded() throws -> Data {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.prettyPrinted, .sortedKeys]
        return try encoder.encode(self)
    }

    /// The short stdout summary the launch-argument run prints before
    /// quitting (spec/08 "Debug menu").
    var stdoutSummary: String {
        let name = switch mode {
        case .synthetic: "list-benchmark"
        case .store: "store-benchmark"
        case .search: "search-benchmark"
        }
        var lines = ["\(name): ok=\(ok)"]
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
        if let search {
            lines.append(
                "  last keystroke to first rows: p95 \(format(search.budget.p95Milliseconds)) ms "
                    + "over \(search.lastKeystroke.count) of \(search.expectedSamples) samples, "
                    + "budget \(Int(search.budget.limitMilliseconds)) ms met: "
                    + (search.budget.met.map(String.init) ?? "—")
            )
            if let stages = search.stages {
                lines.append(
                    "  stages p95: query shown \(format(Self.p95(stages.presented))) ms, "
                        + "list opened \(format(Self.p95(stages.opened))) ms"
                )
            }
        }
        return lines.joined(separator: "\n")
    }

    /// A statistic's p95, or none for an empty one.
    private static func p95(_ statistics: SampleStatistics) -> Double? {
        statistics.count > 0 ? statistics.p95 : nil
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

    /// Window opening → render-ready; store mode uses the nonempty switches' p95.
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
    var display: BenchmarkDisplay?
}

/// Per-run display metadata; a changed display invalidates the measurement.
struct BenchmarkDisplay: Codable, Equatable {
    var id: UInt32?
    var refreshRateHertz: Double
    var changed: Bool
}

enum BenchmarkMode: String, Codable {
    case synthetic
    case store
    /// Last keystroke to first rows in Search (plan M1.6q2).
    case search
}

/// Search mode (plan M1.6q2, spec/08's "Search results after the last
/// keystroke" budget): each query typed into the search field, its last
/// keystroke timed to the first display frame showing the new rows,
/// over the seeded archive M1.6q's runner keeps.
struct SearchBenchmarkResult: Codable, Equatable {
    var queries: [SearchBenchmarkQuery]
    var samples: [SearchBenchmarkSample]
    var expectedSamples: Int
    var lastKeystroke: SampleStatistics
    var perQuery: [String: SampleStatistics]
    var debounceMilliseconds: Int
    var storeMessages: UInt64
    var budget: StoreBenchmarkBudget
    /// Last keystroke to the field showing its query and to the list's
    /// open returning (plan M1.6r2e); first rows are `lastKeystroke`.
    var stages: SearchBenchmarkStages?
}

struct SearchBenchmarkStages: Codable, Equatable {
    var presented: SampleStatistics
    var opened: SampleStatistics
}

/// One query in the field's own text syntax (synthetic words only).
struct SearchBenchmarkQuery: Codable, Equatable {
    var id: String
    var text: String
    /// Whether it runs scoped to the archive's Projects mailbox.
    var projectsScope: Bool
}

/// One timed last keystroke; nil when its rows never rendered.
struct SearchBenchmarkSample: Codable, Equatable {
    var query: String
    var round: Int
    var milliseconds: Double?
    var presentedMilliseconds: Double? = nil
    var openedMilliseconds: Double? = nil
}

/// Foundation-only choices shared by the runner and its tests.
enum SearchBenchmarkPlan {
    /// Rounds through the query set.
    static let rounds = 10
    /// spec/08: Search debounces committed text by 30 ms; the timing
    /// includes it.
    static let debounceMilliseconds = 30
    /// spec/08's budget, as a p95.
    static let limitMilliseconds = 100.0

    /// The query classes M1.6q times in the core, written as typed;
    /// each parses to the core runner's query (the reference word
    /// aside: `ref1` exists in any archive of over 100 messages).
    static let queries = [
        SearchBenchmarkQuery(id: "exact-reference", text: "ref1", projectsScope: false),
        SearchBenchmarkQuery(id: "exact-address", text: "from.is:sender17@beta.example", projectsScope: false),
        SearchBenchmarkQuery(id: "phrase", text: "\"quarterly budget review\"", projectsScope: false),
        SearchBenchmarkQuery(id: "scoped-filter", text: "from.domain:beta.example after:2024-03-01@UTC", projectsScope: true),
        SearchBenchmarkQuery(id: "negative-body", text: "budget -body:review", projectsScope: false),
    ]

    /// Reduce the samples. The budget is judged only on a functionally
    /// valid run with every sample measured.
    static func evaluate(
        samples: [SearchBenchmarkSample], runOK: Bool, storeMessages: UInt64
    ) -> SearchBenchmarkResult {
        let measured = samples.compactMap(\.milliseconds)
        let all = BenchmarkStatistics.summarize(measured)
        var perQuery: [String: SampleStatistics] = [:]
        for query in queries {
            perQuery[query.id] = BenchmarkStatistics.summarize(
                samples.filter { $0.query == query.id }.compactMap(\.milliseconds)
            )
        }
        let expected = queries.count * rounds
        let covered = measured.count == expected
        let p95 = all.count > 0 ? all.p95 : nil
        return SearchBenchmarkResult(
            queries: queries, samples: samples, expectedSamples: expected,
            lastKeystroke: all, perQuery: perQuery,
            debounceMilliseconds: debounceMilliseconds, storeMessages: storeMessages,
            budget: StoreBenchmarkBudget(
                name: "search.lastKeystroke", p95Milliseconds: p95,
                limitMilliseconds: limitMilliseconds,
                met: runOK && covered ? p95.map { $0 < limitMilliseconds } : nil
            ),
            stages: SearchBenchmarkStages(
                presented: BenchmarkStatistics.summarize(samples.compactMap(\.presentedMilliseconds)),
                opened: BenchmarkStatistics.summarize(samples.compactMap(\.openedMilliseconds))
            )
        )
    }
}

/// Store-mode coverage and budgets are separate from functional `ok`.
struct StoreBenchmarkResult: Codable, Equatable {
    var switchEntryIDs: [String]
    var completedSwitches: Int
    var firstRows: SampleStatistics
    var rowCount: Int
    var selectionAnchor: Int
    var selectionEnd: Int
    var selectionSpanReached: Int
    var scrollSpanReached: Int
    var budgets: [StoreBenchmarkBudget]
}

extension StoreBenchmarkResult {
    static func evaluate(
        run: BenchmarkRun, pair: [String], completedSwitches: Int,
        firstRows: SampleStatistics, rowCount: Int
    ) -> StoreBenchmarkResult {
        let endpoints = StoreBenchmarkPlan.selection(count: rowCount)
        let shiftRendered = run.events.contains {
            $0.phase == "selection" && $0.detail.hasPrefix("shift-click")
                && $0.renderReadyIntervalIndex != nil
        }
        let selectionSpan = shiftRendered ? (endpoints?.count ?? 0) : 0
        let jumpRanks = run.events.filter {
            $0.phase == "jumps" && $0.renderReadyIntervalIndex != nil
        }.compactMap { Int($0.detail.split(separator: " ").last ?? "") }
        let scrollSpan = jumpRanks.min().flatMap { low in
            jumpRanks.max().map { $0 - low + 1 }
        } ?? 0
        return StoreBenchmarkResult(
            switchEntryIDs: pair, completedSwitches: completedSwitches,
            firstRows: firstRows, rowCount: rowCount,
            selectionAnchor: endpoints?.lowerBound ?? 0,
            selectionEnd: endpoints?.upperBound ?? 0,
            selectionSpanReached: selectionSpan, scrollSpanReached: scrollSpan,
            budgets: StoreBenchmarkPlan.budgets(
                run: run, firstRows: firstRows, rowCount: rowCount,
                selectionSpan: selectionSpan, scrollSpan: scrollSpan
            )
        )
    }
}

struct StoreBenchmarkBudget: Codable, Equatable {
    var name: String
    var p95Milliseconds: Double?
    var limitMilliseconds: Double
    /// nil means coverage or functional validity was insufficient.
    var met: Bool?
}

/// Foundation-only choices shared by the runner and its tests.
enum StoreBenchmarkPlan {
    static let switchCount = 30
    static let scrollRowTarget = 500_000
    static let selectionRowTarget = 10_000

    struct Mailbox: Equatable {
        var entryID: String
        var count: UInt64
    }

    /// The two largest nonempty mailboxes, ties by entry id; with only
    /// one, it and `fallback` (the Unified Inbox's entry).
    static func switchPair(_ mailboxes: [Mailbox], fallback: String) -> [String] {
        let sorted = mailboxes.filter { $0.count > 0 }.sorted {
            $0.count == $1.count ? $0.entryID < $1.entryID : $0.count > $1.count
        }
        guard let first = sorted.first else { return [] }
        return [first.entryID, sorted.dropFirst().first?.entryID ?? fallback]
    }

    static func selection(count: Int) -> ClosedRange<Int>? {
        guard count > 0 else { return nil }
        let span = min(count, selectionRowTarget)
        let anchor = min(100_000, count - span)
        return anchor ... anchor + span - 1
    }

    static func budgets(
        run: BenchmarkRun, firstRows: SampleStatistics, rowCount: Int,
        selectionSpan: Int, scrollSpan: Int
    ) -> [StoreBenchmarkBudget] {
        func budget(
            _ name: String, _ samples: SampleStatistics,
            limit: Double, covered: Bool
        ) -> StoreBenchmarkBudget {
            let measured = samples.count > 0 ? samples.p95 : nil
            return StoreBenchmarkBudget(
                name: name, p95Milliseconds: measured, limitMilliseconds: limit,
                met: run.ok && covered ? measured.map { $0 < limit } : nil
            )
        }
        let feedback = BenchmarkStatistics.summarize(run.events.filter {
            $0.phase == "selection" && !$0.detail.hasPrefix("position jump")
                && $0.renderReadyIntervalIndex != nil
        }.map(\.latencyMilliseconds))
        return [
            budget("mailbox.firstRows", firstRows, limit: 100,
                   covered: firstRows.count > 0),
            budget("jumps.cpu", run.phases.jumps.cpuMilliseconds, limit: 5,
                   covered: rowCount >= scrollRowTarget && scrollSpan >= scrollRowTarget),
            budget("wheel.cpu", run.phases.wheel.cpuMilliseconds, limit: 5,
                   covered: rowCount >= scrollRowTarget && scrollSpan >= scrollRowTarget),
            budget("selection.feedback", feedback, limit: 50,
                   covered: selectionSpan >= selectionRowTarget),
        ]
    }
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
    /// Display intervals longer than the nominal interval, among this
    /// phase's active intervals.
    var hitchCount: Int
    var eventCount: Int
    /// Events whose expected state was never observed on screen —
    /// left out of the latency percentiles (their latency is only a
    /// lower bound), and they leave a comparison incomplete.
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
    /// Intervals between consecutive samples: the start, each display
    /// tick, the terminal sample.
    var count: Int
    /// Intervals an event's span covered — input or delivery to its
    /// render-ready.
    var activeCount: Int
    /// Display intervals longer than the nominal one, run-wide.
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
    /// never did — `latencyMilliseconds` is then a lower bound, to the
    /// run's end.
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

// Old synthetic reports predate the mode key; their meaning is unchanged.
extension ListBenchmarkReport {
    init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        self.init(
            ok: try values.decode(Bool.self, forKey: .ok),
            failures: try values.decode([String].self, forKey: .failures),
            environment: try values.decode(BenchmarkEnvironment.self, forKey: .environment),
            runs: try values.decode([BenchmarkRun].self, forKey: .runs),
            comparison: try values.decodeIfPresent(BenchmarkComparison.self, forKey: .comparison),
            mode: try values.decodeIfPresent(BenchmarkMode.self, forKey: .mode) ?? .synthetic,
            store: try values.decodeIfPresent(StoreBenchmarkResult.self, forKey: .store),
            search: try values.decodeIfPresent(SearchBenchmarkResult.self, forKey: .search)
        )
    }
}
