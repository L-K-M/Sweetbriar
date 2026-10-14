// spec/07: posts between two frames arrive as one batch on the next
// frame; no frame, no delivery; frames with nothing posted deliver
// nothing.

import Foundation
import Testing
@testable import SweetbriarBridge

/// Yield until `probe` holds, so `post`'s main-actor hops have landed.
private func until(_ probe: () async -> Bool) async {
    while await !probe() {
        await Task.yield()
    }
}

@Test
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
    await until { await clock.waiterCount == 1 }
    #expect(await recorder.batches.isEmpty)

    await clock.tick()
    await until { await recorder.batches.count == 1 }
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
