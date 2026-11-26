// The shared Reading view memory across a store swap: the bridge is the
// same object before and after a restore, so the memory must be dropped
// for the restored archive's senders to load.

import Foundation
import SweetbriarBridge
import Testing

@testable import Sweetbriar

private final class SwapSource: ReadingViewMemorySource, @unchecked Sendable {
    private let lock = NSLock()
    private var held: [String]
    /// The list the store holds; locked, since a load reads it off the
    /// main actor.
    var stored: [String] {
        get { lock.withLock { held } }
        set { lock.withLock { held = newValue } }
    }

    init(_ stored: [String]) {
        held = stored
    }

    func readingViewSenders() async throws -> [String] { stored }

    func setReadingViewSenders(_ senders: [String]) async throws -> Receipt {
        stored = senders
        return Receipt(command: "set_reading_view_senders", resultJson: "{}", createdAtMs: 0)
    }
}

@Suite("Shared reading-view memory")
@MainActor
struct SharedReadingViewMemoryTests {
    @Test func aSourceKeepsItsModelUntilReset() async throws {
        let shared = SharedReadingViewMemory()
        let source = SwapSource([])
        let first = shared.model(source: source)
        #expect(shared.model(source: source) === first)
        shared.reset()
        #expect(shared.model(source: source) !== first)
    }

    @Test func afterAResetTheSameSourceLoadsItsOwnSenders() async throws {
        let shared = SharedReadingViewMemory()
        let source = SwapSource(["before@example.test"])
        let before = shared.model(source: source)
        await before.reload()
        #expect(before.opensReadingView(from: "before@example.test"))

        // The store behind the same bridge is replaced.
        source.stored = ["after@example.test"]
        shared.reset()
        let after = shared.model(source: source)
        #expect(after !== before)
        await after.reload()
        #expect(after.opensReadingView(from: "after@example.test"))
        #expect(!after.opensReadingView(from: "before@example.test"))
    }
}
