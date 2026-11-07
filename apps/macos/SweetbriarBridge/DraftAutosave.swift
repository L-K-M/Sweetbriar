// Compose save scheduling, separate from the revision-aware editor state.
import Foundation

/// The compose window's save owner (spec/08 "Saving"). Call
/// `contentDidChange` after each `DraftModel.update`, and `flush` on blur,
/// close and quit. A false flush keeps the editor open: content is not yet
/// acknowledged. The caller must prevent new edits while deciding to close.
///
/// Only the debounce timer is cancelled. A begun save drains independently
/// of task cancellation and still applies its receipt to the draft model.
/// Use this owner for all of a window's saves, not concurrent direct saves.
@MainActor
public final class DraftAutosave {
    static let delay: Duration = .milliseconds(1_500)

    private let model: DraftModel
    private let sleep: @Sendable (Duration) async throws -> Void
    private let delay: Duration
    private var timer: Task<Void, Never>?
    private var timerVersion: UInt64 = 0
    private var saving: (id: UUID, task: Task<Void, Never>)?
    private var stopped = false

    public convenience init(model: DraftModel) {
        self.init(model: model, delay: Self.delay, sleep: { try await Task.sleep(for: $0) })
    }

    // Tests control deadline delivery, including a cancelled sleeper that
    // nevertheless returns. Timer versions fence those stale deliveries.
    init(
        model: DraftModel, delay: Duration = DraftAutosave.delay,
        sleep: @escaping @Sendable (Duration) async throws -> Void
    ) {
        self.model = model
        self.delay = delay
        self.sleep = sleep
    }

    deinit { timer?.cancel() }

    /// Restart the 1.5 s quiet period. An edit never cancels a save that
    /// already began; its own deadline will save over the acknowledged base.
    public func contentDidChange() {
        cancelTimer()
        guard !stopped, !model.isStopped, !model.isDiscarded, model.isDirty else { return }
        let version = timerVersion
        let sleep = sleep
        let delay = delay
        timer = Task { [weak self] in
            do { try await sleep(delay) } catch { return }
            guard !Task.isCancelled, let self,
                  !self.stopped, self.timerVersion == version else { return }
            self.timer = nil
            _ = await self.save(deadline: version)
        }
    }

    /// Drain any previous save, then save the latest content once if needed.
    /// True means the displayed content has an acknowledged revision. False
    /// includes conflicts, errors, an unloaded/stopped/discarded editor and
    /// edits arriving during this save. Failures never cause a retry loop;
    /// retry explicitly, or resolve the conflict before trying again.
    @discardableResult
    public func flush() async -> Bool {
        cancelTimer()
        return await save(deadline: nil)
    }

    private func save(deadline: UInt64?) async -> Bool {
        guard !stopped else { return false }
        if let active = saving {
            await active.task.value
            if saving?.id == active.id { saving = nil }
        }
        // A timer waiting for an older save cannot pull a newer edit
        // forward out of its own quiet period. Explicit flushes drain it.
        guard deadline == nil || deadline == timerVersion else { return false }
        guard !stopped, !model.isStopped, !model.isDiscarded,
              model.content != nil, model.conflict == nil,
              saving == nil, !model.isSaving, !model.isDiscarding else { return false }
        // A newly created empty draft has no revision yet. Even it needs a
        // receipt before a window may claim its editor content was saved.
        guard model.isDirty || model.baseRevisionId == nil else { return true }
        let id = UUID()
        let model = model
        let task = Task { await model.save() }
        saving = (id, task)
        await task.value
        if saving?.id == id { saving = nil }
        return !stopped && !model.isStopped && !model.isDiscarded
            && !model.isSaving && !model.isDirty && model.baseRevisionId != nil
    }

    /// Retire only the timer. Flush successfully before stopping on close or
    /// quit. This does not stop the draft model or hide a begun save's receipt.
    public func stop() {
        stopped = true
        cancelTimer()
    }

    private func cancelTimer() {
        timerVersion &+= 1
        timer?.cancel()
        timer = nil
    }
}
