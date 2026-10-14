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
