// The bridge's contract (spec/07): settings round-trip through the
// real core, a cancelled call throws `CancellationError` with no
// effect, and calls after shutdown answer `unavailable`.

import Foundation
import Testing
@testable import SweetbriarBridge

@Test
func settingRoundTripThenShutdownAnswersUnavailable() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    _ = try await bridge.setSetting(key: "appearance.compose_cue", value: "strong")

    let settings = try await bridge.settings()
    let cue = try #require(settings.first { $0.key == "appearance.compose_cue" })
    #expect(cue.value == "strong")
    #expect(!cue.isDefault)

    await bridge.shutdown()
    do {
        _ = try await bridge.settings()
        Issue.record("a call after shutdown must throw")
    } catch let failure as CoreFailure {
        guard case .unavailable = failure else {
            Issue.record("expected .unavailable, got \(failure)")
            return
        }
    }
}

@Test
func aCallOnACancelledTaskThrowsAndWritesNothing() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config,
        snapshots: await sinkBatcher(clock: ManualFrameClock())
    )

    // The task parks on the gate, is cancelled, then runs the call:
    // it's already cancelled when `setSetting` installs its handler,
    // so `onCancel` fires the token before the command can begin.
    let gate = Gate()
    let task = Task {
        await gate.wait()
        return try await bridge.setSetting(
            key: "appearance.compose_cue",
            value: "strong"
        )
    }
    task.cancel()
    await gate.open()

    switch await task.result {
    case .success(let receipt):
        Issue.record("expected CancellationError, got \(receipt)")
    case .failure(let error):
        #expect(error is CancellationError, "expected CancellationError, got \(error)")
    }

    let settings = try await bridge.settings()
    let cue = try #require(settings.first { $0.key == "appearance.compose_cue" })
    #expect(cue.isDefault)

    await bridge.shutdown()
}
