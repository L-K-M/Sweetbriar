import Foundation
import Testing

@testable import SweetbriarBridge

private final class MemorySource: ReadingViewMemorySource, @unchecked Sendable {
    var stored: [String]
    var writes = 0
    var error: (any Error)?
    init(_ stored: [String] = [], error: (any Error)? = nil) {
        self.stored = stored
        self.error = error
    }
    func readingViewSenders() async throws -> [String] { stored }
    func setReadingViewSenders(_ senders: [String]) async throws -> Receipt {
        writes += 1
        if let error { throw error }
        stored = senders
        return Receipt(command: "set_setting", resultJson: "{}", createdAtMs: Int64(writes))
    }
}

@Suite @MainActor
struct ReadingViewMemoryTests {
    @Test func canonicalEncodingIsLowercaseSortedUnique() {
        #expect(ReadingViewMemory.encode(["B@x", "a@x", "A@x"]) == "a@x\nb@x")
        // The engine requires byte order; a non-NFC address sorts first.
        #expect(ReadingViewMemory.encode(["z@x", "e\u{301}@x"]) == "e\u{301}@x\nz@x")
        #expect(ReadingViewMemory.decode("") == [])
        #expect(ReadingViewMemory.decode("a@x\nb@x") == ["a@x", "b@x"])
    }

    @Test func addAndRemoveWriteTheCanonicalList() async throws {
        let source = MemorySource()
        let memory = ReadingViewMemory(source: source)
        await memory.reload()
        try await memory.addSender("One@Example")
        #expect(memory.senders == ["one@example"])
        #expect(source.stored == ["one@example"])
        try await memory.addSender("two@example")
        try await memory.removeSender("ONE@example")
        #expect(memory.senders == ["two@example"])
        #expect(source.stored == ["two@example"])
        #expect(source.writes == 3)
        try await memory.removeSender("absent@example")
        #expect(source.writes == 3)
    }

    @Test func rememberedSenderOpensReadingViewAndUnknownDoesNot() async throws {
        let memory = ReadingViewMemory(source: MemorySource(["a@x"]))
        await memory.reload()
        #expect(memory.opensReadingView(from: "A@X"))
        #expect(!memory.opensReadingView(from: "b@x"))
        #expect(!memory.opensReadingView(from: nil))
    }

    @Test func fullListRefusesAnotherSender() async throws {
        let source = MemorySource((0..<1000).map { "u\($0)@x" })
        let memory = ReadingViewMemory(source: source)
        await memory.reload()
        #expect(try await memory.addSender("new@x") == .listFull)
        #expect(memory.senders.count == 1000)
        #expect(source.writes == 0)
    }

    @Test func writeFailureKeepsThePreviousState() async throws {
        let source = MemorySource(["a@x"], error: CoreFailure.unavailable(reason: "offline"))
        let memory = ReadingViewMemory(source: source)
        await memory.reload()
        do {
            _ = try await memory.addSender("b@x")
            Issue.record("The write must fail")
        } catch {}
        #expect(memory.senders == ["a@x"])
        #expect(source.stored == ["a@x"])
    }
}
