import Foundation
import Testing
@testable import SweetbriarBridge

@Test(.timeLimit(.minutes(1)))
func parserQueriesRoundTripExplicitContextThroughTheRealCore() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let context = ParseContext(tz: "UTC", clock: 172_800_000)
    for input in ["is:read", "é OR body:\"a b\"", "on:2026-10-17", "within:1",
                  "from.is:\"\\\"Foo\\\"@EXAMPLE.test\""] {
        let json = try await bridge.parseSearch(text: input, context: context)
        let text = try await bridge.formatSearch(queryJSON: json)
        let reparsed = try await bridge.parseSearch(
            text: text, context: ParseContext(tz: "invalid/unused", clock: .min)
        )
        #expect(reparsed == json)
        if input == "within:1" {
            #expect(text.contains("1@UTC@172800000"))
        }
    }
    let json = try await bridge.parseSearch(
        text: "on:2026-10-17", context: ParseContext(tz: "America/New_York", clock: 0)
    )
    #expect(try await bridge.formatSearch(queryJSON: json).contains("America/New_York"))
    await bridge.shutdown()
}

@Test(.timeLimit(.minutes(1)))
func actualParserFailuresKeepAllCodesAndUtf8OffsetsAcrossTheBridge() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let cases: [(String, UInt32, UInt32, SearchParseCode)] = [
        ("é )", 3, 4, .unexpectedToken),
        ("é AND", 6, 6, .missingValue),
        ("é \"word", 3, 4, .unclosedQuote),
        ("é (word", 3, 4, .unclosedGroup),
        ("é unknown:x", 3, 10, .unknownKey),
        ("é body.is:x", 8, 10, .invalidOperator),
        (#"é "\q""#, 4, 6, .invalidValue),
        ("é on:2026-10-17", 6, 16, .invalidValue),
        (String(repeating: "a", count: 16_383), 0, 16_383, .limit),
    ]
    for (input, start, end, code) in cases {
        do {
            _ = try await bridge.parseSearch(text: input, context: ParseContext(tz: "bad", clock: 0))
            Issue.record("expected typed Search failure")
        } catch let failure as CoreFailure {
            #expect(failure == .searchParse(start: start, end: end, code: code))
        }
    }
    let oversized = String(repeating: " ", count: 16_384) + "😀"
    for query in ParserQuery.allCases {
        do {
            switch query {
            case .parse:
                _ = try await bridge.parseSearch(text: oversized, context: ParseContext(tz: "UTC", clock: 0))
            case .format:
                _ = try await bridge.formatSearch(queryJSON: oversized)
            }
            Issue.record("expected raw UTF8 input limit")
        } catch let failure as CoreFailure {
            #expect(failure == .searchParse(start: 16_384, end: 16_388, code: .limit))
        }
    }
    do {
        _ = try await bridge.formatSearch(queryJSON: "is:read")
        Issue.record("malformed JSON must never fall back to text")
    } catch let failure as CoreFailure {
        #expect(failure == .searchParse(start: 0, end: 1, code: .unexpectedToken))
    }
    await bridge.shutdown()
}

private enum ParserQuery: CaseIterable, Sendable {
    case parse
    case format

    func call(_ bridge: CoreBridge) async throws -> String {
        switch self {
        case .parse:
            return try await bridge.parseSearch(text: "is:read", context: ParseContext(tz: "UTC", clock: 0))
        case .format:
            return try await bridge.formatSearch(queryJSON: #"{"v":1,"q":{"flag":"seen"}}"#)
        }
    }
}

@Test(.timeLimit(.minutes(1)))
func parserTasksCancelBeforeTheirActualCallsAndShutdownRejectsBothQueries() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    for query in ParserQuery.allCases {
        let gate = Gate()
        let task = Task {
            await gate.wait()
            return try await query.call(bridge)
        }
        task.cancel()
        await gate.open()
        switch await task.result {
        case .success:
            Issue.record("expected CancellationError from the actual parser query")
        case .failure(let error):
            #expect(error is CancellationError)
        }
        let healthy = try await query.call(bridge)
        #expect(!healthy.isEmpty)
    }
    await bridge.shutdown()
    for query in ParserQuery.allCases {
        do {
            _ = try await query.call(bridge)
            Issue.record("post-shutdown parser call must throw")
        } catch let failure as CoreFailure {
            guard case .unavailable = failure else {
                Issue.record("expected unavailable, got \(failure)")
                continue
            }
        }
    }
}
