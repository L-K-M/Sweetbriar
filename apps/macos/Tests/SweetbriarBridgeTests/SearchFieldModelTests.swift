import Foundation
import Testing
@testable import SweetbriarBridge

@Suite("Search field state") @MainActor
struct SearchFieldModelTests {
    @Test func spansUseScalarBoundariesRatherThanGraphemeOrByteCounts() {
        let text = "e\u{301}😀x"
        #expect(SearchParseSpan.utf16Range(start: 1, end: 3, in: text) == NSRange(location: 1, length: 1))
        #expect(SearchParseSpan.utf16Range(start: 3, end: 7, in: text) == NSRange(location: 2, length: 2))
        #expect(SearchParseSpan.utf16Range(start: 8, end: 8, in: text) == NSRange(location: 5, length: 0))
        let invalid: [(UInt32, UInt32)] = [(2, 3), (4, 7), (7, 6), (0, 9)]
        for (start, end) in invalid {
            #expect(SearchParseSpan.utf16Range(start: start, end: end, in: text) == nil)
        }
    }

    @Test(.timeLimit(.minutes(1)))
    func actualCoreChipsKeepGroupsNegationNamesIdsAndExplicitDates() async throws {
        try await withFieldCore { bridge in
            let model = SearchFieldModel(parser: bridge, debounce: .zero, label: { reference in
                reference == .mailbox(7) ? "Imported" : nil
            })
            defer { model.close() }
            let input = "from.is:User@EXAMPLE.test (body:x OR body:y) NOT is:read within:7@UTC@172800000 in:7"
            model.updateText(input)
            try await settled(model)
            #expect(model.failure == nil)
            #expect(model.text == input)
            #expect(model.clauses.map(\.isChip) == [true, false, false, true, true])
            let mailbox = try #require(model.clauses.last)
            #expect(mailbox.label == "Imported")
            #expect(mailbox.text.contains("7"))
            let selected = try #require(model.selection(for: mailbox.id))
            #expect((selected.text as NSString).substring(with: selected.range) == mailbox.text)
            let original = try #require(model.queryJSON)
            #expect(try await bridge.parseSearch(text: selected.text, context: .init(tz: "bad", clock: .min)) == original)

            model.moveClause(mailbox.id, toward: .earlier)
            try await settled(model)
            let reordered = try await bridge.parseSearch(
                text: "from.is:User@EXAMPLE.test (body:x OR body:y) NOT is:read in:7 within:7@UTC@172800000",
                context: .init(tz: "bad", clock: .min)
            )
            #expect(model.queryJSON == reordered)
            #expect(model.text.contains("172800000"))
            model.removeClause(mailbox.id) // Old rendered actions cannot edit a new tree.
            #expect(model.queryJSON == reordered && !model.isParsing)
            model.removeClause(try #require(model.clauses.first).id)
            try await settled(model)
            let removed = try await bridge.parseSearch(
                text: "(body:x OR body:y) NOT is:read in:7 within:7@UTC@172800000",
                context: .init(tz: "bad", clock: .min)
            )
            #expect(model.queryJSON == removed)
        }
    }

    @Test(.timeLimit(.minutes(1)))
    func constraintsReuseFrozenQueryAndClearingRestoresPrecedingList() async throws {
        try await withFieldCore { bridge in
            let clock = FieldClock()
            let model = SearchFieldModel(parser: bridge, debounce: .zero,
                                         context: { .init(tz: "UTC", clock: clock.now) })
            defer { model.close() }
            var shown: [(WindowSpec, UInt64)] = []
            model.onPresent = { shown.append(($0, $1)) }
            model.navigated(to: .mailbox(99))
            model.updateText("within:1 in:7")
            try await settled(model)
            let json = try #require(model.queryJSON)
            clock.now += 86_400_000
            model.chooseScope(.account("00000000-0000-0000-0000-000000000001"))
            model.chooseSort(.date)
            #expect(shown.count == 3)
            guard case .search(let final) = shown.last?.0 else { Issue.record("missing Search"); return }
            #expect(final.queryJSON.utf8.elementsEqual(json.utf8))
            #expect(final.sort == .date)
            #expect(final.scope == .account("00000000-0000-0000-0000-000000000001"))
            #expect(!model.isCurrent(shown[0].1) && model.isCurrent(shown[2].1))
            #expect(model.clauses.contains { $0.text.hasPrefix("in:") })
            model.chooseScope(.all)
            #expect(model.clauses.contains { $0.text.hasPrefix("in:") })
            model.clear()
            #expect(shown.last?.0 == .mailbox(99))
            #expect(model.queryJSON == nil && model.clauses.isEmpty && model.text.isEmpty)
        }
    }

    @Test(.timeLimit(.minutes(1)))
    func allActualParseCodesKeepLastValidResultsAndNativeSpans() async throws {
        try await withFieldCore { bridge in
            let model = SearchFieldModel(parser: bridge, debounce: .zero,
                                         context: { .init(tz: "UTC", clock: 0) })
            defer { model.close() }
            var shown = 0
            model.onPresent = { _, _ in shown += 1 }
            model.updateText("is:read")
            try await settled(model)
            let previous = model.queryJSON
            let cases: [(String, UInt32, UInt32, SearchParseCode)] = [
                ("é )", 3, 4, .unexpectedToken), ("é AND", 6, 6, .missingValue),
                ("é \"word", 3, 4, .unclosedQuote), ("é (word", 3, 4, .unclosedGroup),
                ("é unknown:x", 3, 10, .unknownKey), ("é body.is:x", 8, 10, .invalidOperator),
                (#"é "\q""#, 4, 6, .invalidValue),
                (String(repeating: "a", count: 16_383), 0, 16_383, .limit),
            ]
            for (input, start, end, code) in cases {
                model.updateText(input)
                try await settled(model)
                #expect(model.failure == .searchParse(start: start, end: end, code: code))
                #expect(model.errorRange == SearchParseSpan.utf16Range(start: start, end: end, in: input))
                #expect(model.queryJSON == previous && model.lastValidText == "is:read")
                #expect(shown == 1 && !model.canEditClauses)
            }
        }
    }

    @Test(.timeLimit(.minutes(1)))
    func clauseProjectionKeepsInt64PrecisionAndUnicodeIdentity() async throws {
        try await withFieldCore { bridge in
            let model = SearchFieldModel(parser: bridge, debounce: .zero)
            defer { model.close() }
            model.updateText("within:1@UTC@9223372036854775807 in:9223372036854775807")
            try await settled(model)
            #expect(model.failure == nil)
            model.moveClause(try #require(model.clauses.last).id, toward: .earlier)
            try await settled(model)
            let expected = try await bridge.parseSearch(
                text: "in:9223372036854775807 within:1@UTC@9223372036854775807",
                context: .init(tz: "bad", clock: .min)
            )
            #expect(model.queryJSON == expected)
            var specs: [SearchSpec] = []
            model.onPresent = { value, _ in if case .search(let spec) = value { specs.append(spec) } }
            for local in ["é", "e\u{301}"] {
                model.updateText("from.is:\"\(local)@EXAMPLE.test\"")
                try await settled(model)
            }
            #expect(specs.count == 2)
            #expect(specs[0] != specs[1])
            #expect(!specs[0].queryJSON.utf8.elementsEqual(specs[1].queryJSON.utf8))
        }
    }

    @Test(.timeLimit(.minutes(1)))
    func cancelledParsesAndFormatsCannotReplaceANewerQuery() async throws {
        for phase in FieldParserPause.allCases {
            let parser = HeldFieldParser(phase: phase)
            let model = SearchFieldModel(parser: parser, debounce: .zero)
            defer { model.close() }
            var shown: [WindowSpec] = []
            model.onPresent = { value, _ in shown.append(value) }
            model.updateText("old")
            try #require(await eventually { await parser.isWaiting("old") })
            let old = try #require(model.pending)
            model.updateText("new")
            if phase == .parse {
                try #require(await eventually { await parser.isWaiting("new") })
                await parser.release("new")
            }
            try await settled(model)
            let current = model.queryJSON
            let clauses = model.clauses
            await parser.release("old")
            await old.value
            // A parsed query shows before its chips format (M1.6r2e), so a
            // held format has already shown "old"; nothing late replaces "new".
            #expect(model.queryJSON == current && model.text == "new")
            #expect(model.clauses == clauses && clauses.map(\.text) == ["is:draft"])
            #expect(shown.count == (phase == .parse ? 1 : 2))
            guard case .search(let last)? = shown.last else {
                Issue.record("no search was shown")
                return
            }
            #expect(last.queryJSON == current)
            #expect(await eventually { await parser.cancellations.contains("old") })
        }
    }

    @Test(.timeLimit(.minutes(1)))
    func aParsedQueryShowsBeforeItsChipsFormat() async throws {
        let parser = HeldFieldParser(phase: .format)
        let model = SearchFieldModel(parser: parser, debounce: .zero)
        defer { model.close() }
        var shown: [WindowSpec] = []
        model.onPresent = { value, _ in shown.append(value) }
        model.updateText("old")
        try #require(await eventually { await parser.isWaiting("old") })
        // Plan M1.6r2e: the results open while the chips still format,
        // and clause edits wait for them.
        #expect(shown.count == 1 && model.lastValidText == "old")
        #expect(model.isParsing && !model.canEditClauses && model.clauses.isEmpty)
        await parser.release("old")
        // The single clause formats on its own, held the same way.
        try #require(await eventually { await parser.isWaiting("old") })
        await parser.release("old")
        try await settled(model)
        #expect(model.clauses.map(\.text) == ["is:read"] && model.canEditClauses)
        #expect(shown.count == 1)
    }

    @Test(.timeLimit(.minutes(1)))
    func externalNavigationClearAndCloseInvalidatePendingFieldWork() async throws {
        for action in 0..<3 {
            let parser = HeldFieldParser(phase: .parse)
            let model = SearchFieldModel(parser: parser, debounce: .zero)
            var shown: [WindowSpec] = []
            model.onPresent = { value, _ in shown.append(value) }
            model.updateText("old")
            try #require(await eventually { await parser.isWaiting("old") })
            let old = try #require(model.pending)
            switch action {
            case 0: model.navigated(to: .flagged)
            case 1: model.clear()
            default: model.close()
            }
            await parser.release("old")
            await old.value
            #expect(model.queryJSON == nil && shown.isEmpty)
            #expect(await eventually { await parser.cancellations.contains("old") })
            model.close()
        }
    }
}

@MainActor private func settled(_ model: SearchFieldModel) async throws {
    try #require(await eventually { await MainActor.run { !model.isParsing } })
}

@MainActor private func withFieldCore(_ body: @MainActor (CoreBridge) async throws -> Void) async throws {
    let (directory, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: directory) }
    let bridge = try await CoreBridge.open(config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
    do { try await body(bridge) }
    catch { await bridge.shutdown(); throw error }
    await bridge.shutdown()
}

private enum FieldParserPause: CaseIterable, Sendable { case parse, format }

@MainActor private final class FieldClock {
    var now: Int64 = 172_800_000
}

/// Deliberately returns a late success despite cancellation, as a racing native
/// completion can. Tests join the old task before checking for stale effects.
private actor HeldFieldParser: SearchParsing {
    private let phase: FieldParserPause
    private var waiting: [String: CheckedContinuation<String, Never>] = [:]
    private(set) var cancellations: [String] = []

    init(phase: FieldParserPause) { self.phase = phase }
    func isWaiting(_ key: String) -> Bool { waiting[key] != nil }
    private func json(_ key: String) -> String {
        let flag = key == "old" ? "seen" : "draft"
        return #"{"v":1,"q":{"flag":"\#(flag)"}}"#
    }

    func parseSearch(text: String, context: ParseContext) async throws -> String {
        if phase == .parse { return await hold(text) }
        return json(text)
    }

    func formatSearch(queryJSON: String) async throws -> String {
        if queryJSON.contains("seen") && phase == .format { return await hold("old") }
        return queryJSON.contains("seen") ? "is:read" : "is:draft"
    }

    private func hold(_ key: String) async -> String {
        await withTaskCancellationHandler {
            await withCheckedContinuation { waiting[key] = $0 }
        } onCancel: {
            Task { await self.cancelled(key) }
        }
    }

    private func cancelled(_ key: String) { cancellations.append(key) }
    func release(_ key: String) {
        waiting.removeValue(forKey: key)?.resume(returning: phase == .parse ? json(key) : "is:read")
    }
}
