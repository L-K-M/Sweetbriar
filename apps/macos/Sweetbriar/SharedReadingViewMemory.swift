import SweetbriarBridge

/// One `ReadingViewMemory` per core session, shared by every reader
/// (spec/08 "Reading view"): a remembered sender survives the reader's
/// recreation, and a new store loads its own list. Mirrors
/// `SharedAppearance`'s one-model-per-source ownership.
@MainActor
final class SharedReadingViewMemory {
    private var current: ReadingViewMemory?
    private var sourceID: ObjectIdentifier?

    /// The shared owner for `source`. Its list loads in the background,
    /// so installing the archive view never waits on a settings read; a
    /// message shown before the load finishes opens in sender layout. A
    /// failed read leaves an empty list; writes report their own errors
    /// to the reader.
    func model(source: any ReadingViewMemorySource) -> ReadingViewMemory {
        let identity = ObjectIdentifier(source)
        if sourceID == identity, let current { return current }
        let memory = ReadingViewMemory(source: source)
        Task { await memory.reload() }
        sourceID = identity
        current = memory
        return memory
    }

    /// A restored store has its own senders, and the bridge outlives
    /// the swap: drop the shared model so the next reader loads the
    /// restored list, as `SharedAppearance.reset()` does.
    func reset() {
        current = nil
        sourceID = nil
    }
}
