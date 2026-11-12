import Foundation
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@MainActor
private final class EditorProbe {
    var writes = 0
    var began = false
    var finished = false
    var cancelled = false
    var quitBegan = false
    var quitFinished = false
    private var waiter: CheckedContinuation<Void, Never>?

    func park() async {
        await withCheckedContinuation {
            waiter = $0
            began = true
        }
    }
    func release() {
        waiter?.resume()
        waiter = nil
    }
}

@MainActor
private func editorEventually(_ test: @MainActor () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(5)
    while !test() {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

@Suite(.timeLimit(.minutes(1)))
@MainActor
struct SettingsEditorTasksTests {
    @Test func queuedAndStaleControlsCannotWriteAfterRetirement() async {
        let editor = ArchiveWriteTasks()
        let probe = EditorProbe()
        let old = editor.revision
        #expect(editor.run(revision: old) { probe.writes += 1 })
        // No actor suspension before retiring: the scheduled save never began.
        editor.retire()
        #expect(!editor.run(revision: old) { probe.writes += 1 })
        await editor.cancelAndWait()
        #expect(probe.writes == 0)
        #expect(!editor.run(revision: editor.revision) { probe.writes += 1 })
    }

    @Test func freshEditorCanWriteButQuitAwaitsAnAlreadyBegunOldCall() async throws {
        let editor = ArchiveWriteTasks()
        let probe = EditorProbe()
        do {
            #expect(
                editor.run(revision: editor.revision) {
                    await probe.park()
                    probe.cancelled = Task.isCancelled
                    probe.finished = true
                })
            try #require(await editorEventually { probe.began })
            let old = editor.revision
            editor.retire()
            #expect(!editor.run(revision: old) { probe.writes += 100 })
            #expect(editor.run(revision: editor.revision) { probe.writes += 1 })
            try #require(await editorEventually { probe.writes == 1 })
            let quit = Task {
                probe.quitBegan = true
                await editor.cancelAndWait()
                probe.quitFinished = true
            }
            try #require(await editorEventually { probe.quitBegan })
            #expect(!probe.quitFinished && !probe.finished)
            probe.release()
            await quit.value
            #expect(probe.quitFinished && probe.finished && probe.cancelled)
            #expect(probe.writes == 1)
        } catch {
            probe.release()
            await editor.cancelAndWait()
            throw error
        }
    }
}
