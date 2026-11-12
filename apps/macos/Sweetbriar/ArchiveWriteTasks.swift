import Observation

/// Fence UI writes synchronously when restore is admitted, before SwiftUI's
/// next frame removes the editor. An old sheet/button cannot submit a save
/// against the replacement store, even if its view has not disappeared yet.
@MainActor
@Observable
final class ArchiveWriteTasks {
    private(set) var revision: UInt64 = 0
    private var current = ArchiveViewTasks()
    private var retired: [ArchiveViewTasks] = []
    private var stopping = false

    @discardableResult
    func run(revision: UInt64, _ work: @escaping @MainActor () async -> Void) -> Bool {
        guard !stopping, revision == self.revision else { return false }
        if current.isRetired {
            retired.append(current)
            current = ArchiveViewTasks()
        }
        let tasks = current
        tasks.run {
            guard !Task.isCancelled, !tasks.isRetired else { return }
            await work()
        }
        return true
    }

    /// Begun native commands still answer their receipt. Their Core read guard
    /// orders them before a consuming restore; cancellation refuses unbegun ones.
    func retire() {
        revision &+= 1
        current.retire()
    }

    func waitForRetirement() async {
        let draining = retired + [current]
        for tasks in draining { await tasks.waitForRetirement() }
        let drained = Set(draining.map(ObjectIdentifier.init))
        retired.removeAll { drained.contains(ObjectIdentifier($0)) }
    }

    func cancelAndWait() async {
        stopping = true
        retire()
        await waitForRetirement()
    }
}
