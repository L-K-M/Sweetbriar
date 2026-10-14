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

@Test(.timeLimit(.minutes(1)))
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

/// Cancelling a task whose call is already in flight cancels the
/// call's token (spec/07): the work waits for the token the way a
/// query waits in the core, so only `onCancel` can release it.
@Test(.timeLimit(.minutes(1)))
func cancellingACallInFlightCancelsItsToken() async throws {
    let token = RecordingToken()
    let entered = Gate()
    let task = Task {
        try await CoreBridge.withCancellation(token: token) { () async throws -> Bool in
            await entered.open()
            guard await eventually({ token.isCancelled }) else {
                return false
            }
            throw CancellationError()
        }
    }

    await entered.wait()
    #expect(!token.isCancelled, "the token was cancelled before the task was")
    task.cancel()

    let result = await task.result
    #expect(token.isCancelled, "cancelling the task didn't cancel the call's token")
    guard case .failure(let error) = result else {
        Issue.record("the call ran on without its token being cancelled")
        return
    }
    #expect(error is CancellationError, "expected CancellationError, got \(error)")
}

/// A shutdown that arrives while the open is still running waits for
/// it and closes the core it produced (spec/07 "Lifecycle"): the
/// session's core then answers `unavailable`, and its lease is gone —
/// the store opens again.
@Test(.timeLimit(.minutes(1)))
func aShutdownDuringTheOpenClosesWhatTheOpenProduced() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let sink = await sinkBatcher(clock: ManualFrameClock())

    let session = CoreSession {
        try await CoreBridge.open(config: config, snapshots: sink)
    }
    await session.shutdown()

    let opened = try await session.bridge()
    do {
        _ = try await opened.settings()
        Issue.record("the session's core still answered after shutdown")
    } catch let failure as CoreFailure {
        guard case .unavailable = failure else {
            Issue.record("expected .unavailable, got \(failure)")
            return
        }
    }

    let reopened = try await CoreBridge.open(config: config, snapshots: sink)
    await reopened.shutdown()
}
