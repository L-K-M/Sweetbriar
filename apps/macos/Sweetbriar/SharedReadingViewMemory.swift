import SweetbriarBridge

/// One `ReadingViewMemory` per core session, shared by every reader
/// (spec/08 "Reading view"): a remembered sender survives the reader's
/// recreation, and a new store loads its own list. Mirrors
/// `SharedAppearance`'s one-model-per-source ownership.
@MainActor
final class SharedReadingViewMemory {
    private var current: ReadingViewMemory?
    private var sourceID: ObjectIdentifier?

    /// The shared owner for `source`, loaded from the registry before
    /// first use. A failed read leaves an empty list; writes report their
    /// own errors to the reader.
    func model(source: any ReadingViewMemorySource) async -> ReadingViewMemory {
        let identity = ObjectIdentifier(source)
        if sourceID == identity, let current { return current }
        let memory = ReadingViewMemory(source: source)
        await memory.reload()
        sourceID = identity
        current = memory
        return memory
    }
}
