// Test seams: a frame clock ticked by hand, a batch sink, a
// cancellable gate, and a temp-dir config each test cleans up.

import Foundation
@testable import SweetbriarBridge

/// A `FrameClock` the test ticks explicitly: waits park in
/// `waiters` until `tick()` resumes them all as one frame.
actor ManualFrameClock: FrameClock {
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func nextFrame() async {
        await withCheckedContinuation { waiters.append($0) }
    }

    /// How many `nextFrame` waits are parked — how a test knows a post
    /// has landed and the batcher is waiting on a frame.
    var waiterCount: Int { waiters.count }

    func tick() {
        let all = waiters
        waiters.removeAll()
        for waiter in all {
            waiter.resume()
        }
    }
}

/// Where a test's batches go, off the main actor.
actor BatchRecorder {
    private(set) var batches: [[Int]] = []

    func record(_ batch: [Int]) {
        batches.append(batch)
    }
}

/// A one-way gate: `wait()` returns once `open()` runs (or at once if
/// it already did).
actor Gate {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if opened {
            return
        }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        opened = true
        for waiter in waiters {
            waiter.resume()
        }
        waiters.removeAll()
    }
}

/// A one-way throwing gate: `wait()` returns once `open()` runs, or
/// throws `CancellationError` if its own task is cancelled first —
/// how a fake read observes that the model dropped it.
actor ThrowingGate {
    private var opened = false
    private var waiters: [UUID: CheckedContinuation<Void, any Error>] = [:]
    private var cancelled: Set<UUID> = []

    func wait() async throws {
        if opened {
            return
        }
        let id = UUID()
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation in
                Task { self.park(id, continuation) }
            }
        } onCancel: {
            Task { await self.resumeCancelled(id) }
        }
    }

    /// Park a waiter — or resume it at once when the gate is already
    /// open, or the wait was cancelled before the park landed.
    private func park(
        _ id: UUID,
        _ continuation: CheckedContinuation<Void, any Error>
    ) {
        if opened {
            continuation.resume()
            return
        }
        if cancelled.contains(id) {
            cancelled.remove(id)
            continuation.resume(throwing: CancellationError())
            return
        }
        waiters[id] = continuation
    }

    private func resumeCancelled(_ id: UUID) {
        if let waiter = waiters.removeValue(forKey: id) {
            waiter.resume(throwing: CancellationError())
        } else {
            cancelled.insert(id)
        }
    }

    func open() {
        opened = true
        let all = waiters
        waiters.removeAll()
        for waiter in all.values {
            waiter.resume()
        }
    }
}

/// A fresh temp dir and a config rooted in it; caller removes the dir.
func tempConfig() -> (URL, CoreConfig) {
    let dir = FileManager.default.temporaryDirectory
        .appendingPathComponent("sweetbriar-tests-\(UUID().uuidString)", isDirectory: true)
    let config = CoreConfig(
        dataRoot: dir.appendingPathComponent("data").path,
        cacheRoot: dir.appendingPathComponent("cache").path
    )
    return (dir, config)
}

/// A snapshot batcher that drops its batches — for tests that don't
/// exercise batching.
@MainActor
func sinkBatcher(clock: ManualFrameClock) -> FrameBatcher<[Setting]> {
    FrameBatcher(clock: clock) { _ in }
}

/// The longest a test waits for an asynchronous event: a regression
/// fails within it instead of hanging the suite (a test's time limit
/// cancels it, but cancellation can't end a wait that ignores it).
let eventTimeout: Duration = .seconds(10)

/// Poll `probe` until it holds; `false` once `eventTimeout` passes.
/// `@Sendable` keeps a probe written inside a `@MainActor` test
/// nonisolated, so handing it to this nonisolated function is safe.
func eventually(_ probe: @Sendable () async -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + eventTimeout
    while await !probe() {
        if ContinuousClock.now >= deadline {
            return false
        }
        try? await Task.sleep(for: .milliseconds(1))
    }
    return true
}

/// A stand-in for the core's token: records `cancel()`, so a test's
/// work can wait for it the way a query waits in the core.
final class RecordingToken: CallToken, @unchecked Sendable {
    private let lock = NSLock()
    private var cancelled = false

    var isCancelled: Bool {
        lock.withLock { cancelled }
    }

    func cancel() {
        lock.withLock { cancelled = true }
    }
}
