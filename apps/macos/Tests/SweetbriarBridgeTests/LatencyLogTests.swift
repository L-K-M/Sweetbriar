// The opt-in latency log and its report (spec/08 "Latency log").

import Foundation
import Testing

@testable import SweetbriarBridge

@MainActor
@Suite("Latency log")
struct LatencyLogTests {
    private func scratch() throws -> URL {
        let url = FileManager.default.temporaryDirectory
            .appendingPathComponent("sb-latency-\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        return url
    }

    private func entry(
        _ interaction: LatencyInteraction, _ milliseconds: Double, warm: Bool? = nil
    ) -> LatencyEntry {
        LatencyEntry(
            interaction: interaction, milliseconds: milliseconds, endedAtMs: 1_700_000_000_000,
            appVersion: "0.0.1", warm: warm)
    }

    /// An entry is one JSON line of a name, a duration, a time and the
    /// version, and reads back as written.
    @Test func entriesAreOneLineEachAndReadBack() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let log = LatencyLog(directory: root.appendingPathComponent("diagnostics"))
        log.record(entry(.search, 42.5))
        log.record(entry(.launch, 300, warm: true))

        let text = try String(contentsOf: log.fileURL, encoding: .utf8)
        #expect(
            text == """
                {"app_version":"0.0.1","ended_at_ms":1700000000000,"interaction":"search","milliseconds":42.5}
                {"app_version":"0.0.1","ended_at_ms":1700000000000,"interaction":"launch","milliseconds":300,"warm":true}

                """)
        let read = try log.read()
        #expect(read.entries == [entry(.search, 42.5), entry(.launch, 300, warm: true)])
        #expect(read.unreadable == 0)
    }

    /// Past the capacity the oldest lines go first, also across a new
    /// log over the same file.
    @Test func theOldestLinesAreDroppedFirst() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let log = LatencyLog(directory: root, capacity: 3)
        for value in 1...4 { log.record(entry(.search, Double(value))) }
        #expect(try log.read().entries.map(\.milliseconds) == [2, 3, 4])

        let reopened = LatencyLog(directory: root, capacity: 3)
        reopened.record(entry(.search, 5))
        #expect(try reopened.read().entries.map(\.milliseconds) == [3, 4, 5])
    }

    /// A full log drops its oldest tenth at once, not one line per entry.
    @Test func aFullLogDropsItsOldestTenth() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let log = LatencyLog(directory: root, capacity: 10)
        for value in 1...11 { log.record(entry(.search, Double(value))) }
        #expect(try log.read().entries.map(\.milliseconds) == (3...11).map(Double.init))
        log.record(entry(.search, 12))
        #expect(try log.read().entries.count == 10)
        log.record(entry(.search, 13))
        #expect(try log.read().entries.map(\.milliseconds) == (5...13).map(Double.init))
    }

    /// A failed write loses the entry, never the interaction: the log
    /// counts it and the report says why.
    @Test func aFailedWriteIsCountedAndReported() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let blocker = root.appendingPathComponent("diagnostics")
        try Data().write(to: blocker)
        let log = LatencyLog(directory: blocker)
        log.record(entry(.search, 1))
        log.record(entry(.search, 2))

        #expect(log.lostEntries == 2)
        #expect(log.lastFailure != nil)
        let report = log.report()
        #expect(report.lostEntries == 2)
        #expect(report.text.contains("Entries lost to write failures: 2"))
    }

    /// A line that doesn't decode is counted, not fatal.
    @Test func anUnreadableLineIsCounted() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let log = LatencyLog(directory: root)
        log.record(entry(.search, 1))
        let handle = try FileHandle(forWritingTo: log.fileURL)
        try handle.seekToEnd()
        try handle.write(contentsOf: Data("not json\n".utf8))
        try handle.close()

        let read = try log.read()
        #expect(read.entries.count == 1)
        #expect(read.unreadable == 1)
        #expect(log.report().unreadableLines == 1)
    }

    @Test func clearingEmptiesTheLog() throws {
        let root = try scratch()
        defer { try? FileManager.default.removeItem(at: root) }
        let log = LatencyLog(directory: root)
        log.record(entry(.search, 1))
        try log.clear()
        #expect(try log.read().entries.isEmpty)
        log.record(entry(.search, 2))
        #expect(try log.read().entries.map(\.milliseconds) == [2])
    }

    /// Nearest-rank percentiles against each budget; only warm launches
    /// count toward the launch budget.
    @Test func theReportComparesEachP95WithItsBudget() {
        var entries = (1...20).map { entry(.search, Double($0 * 5)) }
        entries += [entry(.launch, 1_000, warm: false), entry(.launch, 350, warm: true)]
        entries += [entry(.archiveVisible, 60)]
        let report = LatencyReport(entries: entries, unreadableLines: 0, lostEntries: 0, lastFailure: nil)

        let search = report.rows.first { $0.interaction == .search }
        #expect(search?.count == 20)
        #expect(search?.p50 == 50)
        #expect(search?.p95 == 95)
        #expect(search?.max == 100)
        #expect(search?.meetsBudget == true)
        let launch = report.rows.first { $0.interaction == .launch }
        #expect(launch?.count == 1)
        #expect(launch?.p95 == 350)
        #expect(launch?.meetsBudget == true)
        #expect(report.coldLaunches == 1)
        #expect(report.rows.first { $0.interaction == .archiveVisible }?.meetsBudget == false)
        #expect(report.rows.first { $0.interaction == .openCompose }?.meetsBudget == nil)
        #expect(report.rows.map(\.interaction) == LatencyInteraction.allCases)
        #expect(report.text.contains("search: 20 samples, p50 50.0 ms, p95 95.0 ms, max 100.0 ms; budget 100.0 ms: meets"))
        #expect(report.text.contains("archive_visible: 1 samples"))
        #expect(report.text.contains("misses"))
    }

    /// spec/08's budgets, in milliseconds.
    @Test func theBudgetsAreSpec08s() {
        #expect(LatencyInteraction.launch.budgetMilliseconds == 400)
        #expect(LatencyInteraction.archiveVisible.budgetMilliseconds == 50)
        #expect(LatencyInteraction.deleteVisible.budgetMilliseconds == 50)
        for interaction in [LatencyInteraction.openMessage, .archiveDurable, .deleteDurable, .search, .openCompose] {
            #expect(interaction.budgetMilliseconds == 100)
        }
    }
}
