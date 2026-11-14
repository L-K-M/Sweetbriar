import SweetbriarBridge

/// One preference cohort per stable bridge. Restore freezes synchronously;
/// retirement stays owned even when its initiating waiter is cancelled.
@MainActor
final class SharedComposeCue {
    private let canWrite: @MainActor () -> Bool
    private var current: ComposeCueModel?
    private var sourceID: ObjectIdentifier?
    private var initialLoad: Task<Void, Never>?
    private var retirement: Task<Void, Never>?
    private var epoch: UInt64 = 0
    private var frozen = false
    private var stopped = false

    init(canWrite: @escaping @MainActor () -> Bool) {
        self.canWrite = canWrite
    }

    /// A different source requires an explicit awaited reset. No fallback
    /// model may start a query while an old context is retiring.
    func model(source: any ComposeCueSource) -> ComposeCueModel? {
        guard !frozen, !stopped, retirement == nil else { return nil }
        if let current {
            guard sourceID == ObjectIdentifier(source) else { return nil }
            return current
        }
        let model = ComposeCueModel(
            source: source,
            canWrite: { [weak self] in
                guard let self, !self.frozen, !self.stopped else { return false }
                return self.canWrite()
            })
        current = model
        sourceID = ObjectIdentifier(source)
        initialLoad = Task {
            guard !Task.isCancelled else { return }
            await model.reload()
        }
        return model
    }

    func freezeForRestore() {
        epoch &+= 1
        frozen = true
        _ = retireCurrent()
    }

    /// Drain the frozen cohort before the native restore may replace its store.
    func waitForRetirement() async {
        await retirement?.value
    }

    /// Reopening is explicit and follows the actual restore outcome. A newer
    /// freeze or permanent stop must not be undone by this older waiter.
    func reset() async {
        let ticket = epoch
        frozen = true
        await retireCurrent()?.value
        guard !stopped, ticket == epoch else { return }
        frozen = false
    }

    /// Called synchronously only after compose quit has been accepted.
    func stop() {
        epoch &+= 1
        stopped = true
        frozen = true
        _ = retireCurrent()
    }

    func stopAndWait() async {
        stop()
        await retirement?.value
    }

    private func retireCurrent() -> Task<Void, Never>? {
        if let retirement { return retirement }
        guard let model = current else { return nil }
        let load = initialLoad
        load?.cancel()
        let task = Task { [self] in
            // Cancelling the load waiter alone does not cancel the model's
            // separately owned native query. Stop it before joining the waiter.
            await model.stopAndWait()
            await load?.value
            current = nil
            sourceID = nil
            initialLoad = nil
            retirement = nil
        }
        retirement = task
        return task
    }
}
