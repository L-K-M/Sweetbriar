// `CoreSession`: the app's one core across its life (spec/07
// "Lifecycle"). The open starts at once in the background, and
// `shutdown()` closes whatever the open produced — also when quit
// arrives while the open is still running.

/// Owns the core's open and its shutdown, so the core never outlives
/// the app unclosed.
///
/// `shutdown()` waits for a running open to finish and then shuts down
/// the core it produced; a failed open leaves nothing to close.
public final class CoreSession: Sendable {
    private let opening: Task<CoreBridge, any Error>

    /// Start opening with `open`, e.g.
    /// `CoreSession { try await CoreBridge.open(snapshots: batcher) }`.
    public init(open: @escaping @Sendable () async throws -> CoreBridge) {
        opening = Task {
            try await open()
        }
    }

    /// The open core once the open finishes; the open's error if it
    /// failed.
    public func bridge() async throws -> CoreBridge {
        try await opening.value
    }

    /// Wait for the open, then shut down whatever it opened.
    public func shutdown() async {
        guard let bridge = try? await opening.value else {
            return
        }
        await bridge.shutdown()
    }
}
