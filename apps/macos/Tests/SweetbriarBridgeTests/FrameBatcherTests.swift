// spec/07: posts between two frames arrive as one batch on the next
// frame; no frame, no delivery; frames with nothing posted deliver
// nothing.

import Foundation
import Testing
@testable import SweetbriarBridge

@Test(.timeLimit(.minutes(1)))
func postsBetweenFramesArriveAsOneBatch() async throws {
    let clock = ManualFrameClock()
    let recorder = BatchRecorder()
    let batcher = await FrameBatcher<Int>(clock: clock) { batch in
        Task {
            await recorder.record(batch)
        }
    }

    batcher.post(1)
    batcher.post(2)
    batcher.post(3)

    // Posts hop to the main actor; wait until the batcher is parked on
    // a frame before checking nothing arrived early.
    try #require(await eventually { await clock.waiterCount == 1 })
    #expect(await recorder.batches.isEmpty)

    await clock.tick()
    try #require(await eventually { await recorder.batches.count == 1 })
    let batch = try #require(await recorder.batches.first)
    #expect(batch.sorted() == [1, 2, 3])

    // Two frames with nothing pending deliver no empty batch.
    await clock.tick()
    await clock.tick()
    for _ in 0 ..< 16 {
        await Task.yield()
    }
    #expect(await recorder.batches.count == 1)
}

/// Each frame delivers exactly what was posted since the previous
/// one: a second burst after a delivery waits for its own frame and
/// arrives whole, and nothing is stranded after the first delivery.
@Test(.timeLimit(.minutes(1)))
func eachFrameDeliversWhatWasPostedSinceThePrevious() async throws {
    let clock = ManualFrameClock()
    let recorder = BatchRecorder()
    let batcher = await FrameBatcher<Int>(clock: clock) { batch in
        Task {
            await recorder.record(batch)
        }
    }

    batcher.post(1)
    batcher.post(2)
    try #require(await eventually { await clock.waiterCount == 1 })
    await clock.tick()
    try #require(await eventually { await recorder.batches.count == 1 })

    batcher.post(3)
    batcher.post(4)
    try #require(await eventually { await clock.waiterCount == 1 })
    #expect(await recorder.batches.count == 1, "the second burst arrived before its frame")

    await clock.tick()
    try #require(await eventually { await recorder.batches.count == 2 })
    let batches = await recorder.batches
    #expect(batches[0].sorted() == [1, 2])
    #expect(batches[1].sorted() == [3, 4])
}
