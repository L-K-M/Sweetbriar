// `health()` and its subscription across the bridge (spec/07; spec/03
// "Integrity"): a fresh store is sound; a store whose latch can't be
// read reopens read-only, says why, and refuses a writing command;
// a waiting `next` is cancellable.

import Foundation
import Testing
@testable import SweetbriarBridge

@Test
func healthReportsALatchedStoreAndItsSubscriptionCancels() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    #expect(try await bridge.health() == Health(integrity: .sound))
    await bridge.shutdown()

    // A latch that can't be read is still a latch (spec/03).
    let latch = URL(fileURLWithPath: config.dataRoot)
        .appendingPathComponent("integrity-latch.json")
    try Data("not json".utf8).write(to: latch)

    let latched = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let health = try await latched.health()
    guard case .readOnly(let detail, let detectedAt, let copyPath) = health.integrity else {
        Issue.record("expected .readOnly, got \(health.integrity)")
        return
    }
    #expect(detail.contains("can't be read"))
    #expect(detectedAt == nil)
    #expect(copyPath == nil)

    // Queries go on; a command that writes fails `corrupt`.
    _ = try await latched.settings()
    do {
        _ = try await latched.setSetting(key: "appearance.compose_cue", value: "strong")
        Issue.record("a write on a read-only store must throw")
    } catch let failure as CoreFailure {
        guard case .corrupt = failure else {
            Issue.record("expected .corrupt, got \(failure)")
            return
        }
    }

    // Nothing changes on a latched store: `next` waits until its task
    // is cancelled.
    let changes = try await latched.subscribeHealth()
    let waiting = Task { try await changes.next() }
    waiting.cancel()
    do {
        try await waiting.value
        Issue.record("a cancelled next must throw")
    } catch is CancellationError {
    }

    await latched.shutdown()
}
