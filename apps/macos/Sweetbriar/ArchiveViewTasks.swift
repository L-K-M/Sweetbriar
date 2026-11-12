import Foundation

/// Calls owned by one incarnation of an archive view. Retire before tearing
/// down its models, then await all late opens/releases before creating new UI.
@MainActor
final class ArchiveViewTasks {
    private(set) var isRetired = false
    private var tasks: [UUID: Task<Void, Never>] = [:]

    @discardableResult
    func run(_ work: @escaping @MainActor () async -> Void) -> Task<Void, Never> {
        guard !isRetired else { return Task {} }
        let id = UUID()
        let task = Task { [weak self] in
            await work()
            self?.tasks[id] = nil
        }
        tasks[id] = task
        return task
    }

    /// Prevent admission before requesting cancellation. Native commands that
    /// already began still run to their answer; cancellation never drops them.
    func retire() {
        isRetired = true
        for task in tasks.values { task.cancel() }
    }

    /// Called by the owner, never from a task owned by this collection.
    func waitForRetirement() async {
        precondition(isRetired, "retire archive-view tasks before awaiting them")
        for task in Array(tasks.values) { await task.value }
    }

    deinit {
        for task in tasks.values { task.cancel() }
    }
}
