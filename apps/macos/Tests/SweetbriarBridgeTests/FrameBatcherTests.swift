// spec/07: a batch posted after an idle frame lands at once; posts
// after it, until the next frame, arrive as one batch on that frame;
// no frame, no further delivery; frames with nothing posted deliver
// nothing.

import Foundation
import Testing
@testable import SweetbriarBridge

/// The first post after an idle frame lands without a tick; the
/// posts after it wait for the next frame and arrive whole on it.
@Test(.timeLimit(.minutes(1)))
func aPostAfterAnIdleFrameLandsAtOnce() async throws {
    let clock = ManualFrameClock()
    let recorder = BatchRecorder()
    let batcher = await FrameBatcher<Int>(clock: clock) { batch in
        Task {
            await recorder.record(batch)
        }
    }

    batcher.post(1)
    try #require(await eventually { await recorder.batches.count == 1 })
    #expect(await recorder.batches == [[1]])

    batcher.post(2)
    batcher.post(3)
    // Posts hop to the main actor; wait until both joined the pending
    // batch before checking nothing arrived early.
    try #require(await eventually { await batcher.pendingCount == 2 })
    #expect(await recorder.batches.count == 1, "a second batch landed in the same frame")

    await clock.tick()
    try #require(await eventually { await recorder.batches.count == 2 })
    let batches = await recorder.batches
    #expect(batches[1].sorted() == [2, 3])

    // Two frames with nothing pending deliver no empty batch.
    await clock.tick()
    await clock.tick()
    for _ in 0 ..< 16 {
        await Task.yield()
    }
    #expect(await recorder.batches.count == 2)
}

/// A frame that delivered keeps the next post waiting for its own
/// frame; once a frame passes with nothing to deliver, a post lands at
/// once again.
@Test(.timeLimit(.minutes(1)))
func onlyAnIdleFrameLetsAPostLandAtOnce() async throws {
    let clock = ManualFrameClock()
    let recorder = BatchRecorder()
    let batcher = await FrameBatcher<Int>(clock: clock) { batch in
        Task {
            await recorder.record(batch)
        }
    }

    batcher.post(1)
    try #require(await eventually { await recorder.batches.count == 1 })
    batcher.post(2)
    try #require(await eventually { await batcher.pendingCount == 1 })
    await clock.tick()
    try #require(await eventually { await recorder.batches.count == 2 })

    // The frame that just delivered 2 counts as busy: 3 waits for the
    // next one.
    batcher.post(3)
    try #require(await eventually { await batcher.pendingCount == 1 })
    #expect(await recorder.batches.count == 2, "3 landed in the frame that delivered 2")
    await clock.tick()
    try #require(await eventually { await recorder.batches.count == 3 })

    // An idle frame: nothing was pending at this tick.
    try #require(await eventually { await clock.waiterCount == 1 })
    await clock.tick()
    try #require(await eventually { await batcher.isIdle })

    batcher.post(4)
    try #require(await eventually { await recorder.batches.count == 4 })
    #expect(await recorder.batches == [[1], [2], [3], [4]])
}
