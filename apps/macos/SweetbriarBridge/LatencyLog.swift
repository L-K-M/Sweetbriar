// The opt-in local latency log (spec/08 "Latency log"): one JSON line
// per measured interaction in the cache root's `diagnostics/`, and the
// percentile report the Debug menu shows. Foundation only, so the
// bridge's tests exercise it. It never records message content, ids,
// addresses or search text: an entry is a name, a duration, a time and
// the app's version.

import Foundation

/// The interactions spec/08's budgets name.
public enum LatencyInteraction: String, CaseIterable, Codable, Sendable {
    case launch
    case openMessage = "open_message"
    case archiveVisible = "archive_visible"
    case archiveDurable = "archive_durable"
    case deleteVisible = "delete_visible"
    case deleteDurable = "delete_durable"
    case search
    case openCompose = "open_compose"

    /// The spec/08 p95 budget, in milliseconds.
    public var budgetMilliseconds: Double {
        switch self {
        case .launch: 400
        case .archiveVisible, .deleteVisible: 50
        case .openMessage, .archiveDurable, .deleteDurable, .search, .openCompose: 100
        }
    }
}

/// One measured interaction.
public struct LatencyEntry: Codable, Equatable, Sendable {
    public var interaction: LatencyInteraction
    public var milliseconds: Double
    /// When the span ended, in epoch milliseconds.
    public var endedAtMs: Int64
    public var appVersion: String
    /// Launches only: false for the first launch since the Mac started.
    public var warm: Bool?

    public init(
        interaction: LatencyInteraction, milliseconds: Double, endedAtMs: Int64,
        appVersion: String, warm: Bool? = nil
    ) {
        self.interaction = interaction
        self.milliseconds = milliseconds
        self.endedAtMs = endedAtMs
        self.appVersion = appVersion
        self.warm = warm
    }

    enum CodingKeys: String, CodingKey {
        case interaction, milliseconds, warm
        case endedAtMs = "ended_at_ms"
        case appVersion = "app_version"
    }
}

/// The log file. Appending never throws to the interaction it measures:
/// a failed write counts as lost and the report says so (spec/08).
@MainActor
public final class LatencyLog {
    /// The most lines the file keeps; the oldest are dropped first.
    /// Past it the oldest tenth goes at once, so a full log is
    /// rewritten once per thousand entries rather than on each.
    public static let capacity = 10_000
    public static let fileName = "latency.jsonl"

    public let fileURL: URL
    private let capacity: Int
    /// Entries a write failure lost since launch.
    public private(set) var lostEntries = 0
    /// The last write failure's description.
    public private(set) var lastFailure: String?
    /// The file's line count, read once when the first entry is appended.
    private var lineCount: Int?

    /// A log in `directory` (the cache root's `diagnostics/`).
    public init(directory: URL, capacity: Int = LatencyLog.capacity) {
        fileURL = directory.appendingPathComponent(Self.fileName, isDirectory: false)
        self.capacity = capacity
    }

    /// Append one entry, dropping the oldest lines past the capacity.
    public func record(_ entry: LatencyEntry) {
        do {
            let line = try Self.encoder.encode(entry) + Data("\n".utf8)
            let directory = fileURL.deletingLastPathComponent()
            try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
            let count = try lineCount ?? existingLines()
            if count + 1 > capacity {
                let keep = max(capacity - capacity / 10, 1) - 1
                try rewrite(keepingLast: keep, appending: line)
                lineCount = keep + 1
            } else {
                try append(line)
                lineCount = count + 1
            }
        } catch {
            lostEntries += 1
            lastFailure = error.localizedDescription
            lineCount = nil
        }
    }

    /// The entries the file holds, and how many lines didn't decode.
    public func read() throws -> (entries: [LatencyEntry], unreadable: Int) {
        guard FileManager.default.fileExists(atPath: fileURL.path) else { return ([], 0) }
        var entries: [LatencyEntry] = []
        var unreadable = 0
        for line in try Self.lines(of: Data(contentsOf: fileURL)) {
            if let entry = try? Self.decoder.decode(LatencyEntry.self, from: line) {
                entries.append(entry)
            } else {
                unreadable += 1
            }
        }
        return (entries, unreadable)
    }

    /// Debug → Clear Latency Log: empty the file.
    public func clear() throws {
        if FileManager.default.fileExists(atPath: fileURL.path) {
            try Data().write(to: fileURL, options: .atomic)
        }
        lineCount = 0
    }

    /// The report over what the file holds now.
    public func report() -> LatencyReport {
        do {
            let (entries, unreadable) = try read()
            return LatencyReport(
                entries: entries, unreadableLines: unreadable, lostEntries: lostEntries,
                lastFailure: lastFailure)
        } catch {
            return LatencyReport(
                entries: [], unreadableLines: 0, lostEntries: lostEntries,
                lastFailure: lastFailure, readFailure: error.localizedDescription)
        }
    }

    private func existingLines() throws -> Int {
        guard FileManager.default.fileExists(atPath: fileURL.path) else { return 0 }
        return Self.lines(of: try Data(contentsOf: fileURL)).count
    }

    private func append(_ line: Data) throws {
        guard FileManager.default.fileExists(atPath: fileURL.path) else {
            try line.write(to: fileURL)
            return
        }
        let handle = try FileHandle(forWritingTo: fileURL)
        defer { try? handle.close() }
        try handle.seekToEnd()
        try handle.write(contentsOf: line)
    }

    private func rewrite(keepingLast keep: Int, appending line: Data) throws {
        let kept = try Self.lines(of: Data(contentsOf: fileURL)).suffix(keep)
        var data = Data()
        for old in kept {
            data.append(old)
            data.append(UInt8(ascii: "\n"))
        }
        data.append(line)
        try data.write(to: fileURL, options: .atomic)
    }

    private static func lines(of data: Data) -> [Data] {
        data.split(separator: UInt8(ascii: "\n"), omittingEmptySubsequences: true).map { Data($0) }
    }

    private static let encoder: JSONEncoder = {
        let encoder = JSONEncoder()
        encoder.outputFormatting = [.sortedKeys]
        return encoder
    }()
    private static let decoder = JSONDecoder()
}

/// Debug → Latency Report (spec/08): each interaction's count and
/// percentiles beside its budget. Only warm launches count toward the
/// launch budget.
public struct LatencyReport: Equatable, Sendable {
    public struct Row: Equatable, Sendable {
        public var interaction: LatencyInteraction
        public var count: Int
        /// Nearest-rank percentiles, as the benchmarks compute them; nil
        /// without samples.
        public var p50: Double?
        public var p95: Double?
        public var max: Double?
        public var budget: Double
        /// Whether the p95 is within the budget; nil without samples.
        public var meetsBudget: Bool?
    }

    public var rows: [Row]
    /// Launches that weren't warm, which the launch row leaves out.
    public var coldLaunches: Int
    public var unreadableLines: Int
    public var lostEntries: Int
    public var lastFailure: String?
    public var readFailure: String?

    public init(
        entries: [LatencyEntry], unreadableLines: Int, lostEntries: Int, lastFailure: String?,
        readFailure: String? = nil
    ) {
        rows = LatencyInteraction.allCases.map { interaction in
            let samples = entries.filter {
                $0.interaction == interaction && (interaction != .launch || $0.warm == true)
            }.map(\.milliseconds)
            let p95 = Self.percentile(samples, p: 95)
            return Row(
                interaction: interaction, count: samples.count,
                p50: Self.percentile(samples, p: 50), p95: p95, max: samples.max(),
                budget: interaction.budgetMilliseconds,
                meetsBudget: p95.map { $0 <= interaction.budgetMilliseconds })
        }
        coldLaunches = entries.filter { $0.interaction == .launch && $0.warm != true }.count
        self.unreadableLines = unreadableLines
        self.lostEntries = lostEntries
        self.lastFailure = lastFailure
        self.readFailure = readFailure
    }

    /// The nearest-rank percentile of `samples`, nil when empty.
    static func percentile(_ samples: [Double], p: Double) -> Double? {
        guard !samples.isEmpty else { return nil }
        let sorted = samples.sorted()
        let rank = Int((p / 100 * Double(sorted.count)).rounded(.up))
        return sorted[min(max(rank, 1), sorted.count) - 1]
    }

    /// The report as plain text, for the window and its Copy button.
    public var text: String {
        func ms(_ value: Double?) -> String {
            value.map { String(format: "%.1f ms", $0) } ?? "n/a"
        }
        var lines = ["Interaction latency (p95 budgets, spec/08)", ""]
        for row in rows {
            let verdict =
                switch row.meetsBudget {
                case .some(true): "meets"
                case .some(false): "misses"
                case .none: "no samples"
                }
            lines.append(
                "\(row.interaction.rawValue): \(row.count) samples, p50 \(ms(row.p50)), "
                    + "p95 \(ms(row.p95)), max \(ms(row.max)); budget \(ms(row.budget)): \(verdict)")
        }
        lines.append("")
        lines.append("Cold launches left out of the launch budget: \(coldLaunches)")
        if unreadableLines > 0 { lines.append("Unreadable lines: \(unreadableLines)") }
        if lostEntries > 0 {
            lines.append("Entries lost to write failures: \(lostEntries) (\(lastFailure ?? "unknown"))")
        }
        if let readFailure { lines.append("The log couldn't be read: \(readFailure)") }
        return lines.joined(separator: "\n")
    }
}
