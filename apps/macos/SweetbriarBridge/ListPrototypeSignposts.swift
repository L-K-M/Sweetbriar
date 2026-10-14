// The list prototype's signposts (plan M0.10): one `OSSignposter`,
// subsystem `com.sweetbriar`, category `ListPrototype`, marking
// "input" (the harness), "queue" (a read's request → completion),
// "ffi" (the bridge call) and "apply" (page delivery handling) so an
// Instruments run can break one update down. `os` exists only on
// Apple's platforms, so every hook no-ops off it — the bridge keeps
// building on Linux.

#if canImport(os)
import os
#endif

public enum ListPrototypeSignposts {
    /// A begun interval. `OSSignpostIntervalState` ships no `Sendable`
    /// conformance to cite, so the token crosses a read task's
    /// boundary inside this unchecked box — it is only ever begun and
    /// ended on the main actor.
    public struct Span: @unchecked Sendable {
        #if canImport(os)
        let state: OSSignpostIntervalState
        #endif
    }

    #if canImport(os)
    /// The one signposter every prototype interval runs through
    /// (plan M0.10).
    public static let signposter = OSSignposter(
        subsystem: "com.sweetbriar",
        category: "ListPrototype"
    )

    /// Begin an interval named `name` — "input", "queue", "ffi" or
    /// "apply".
    public static func begin(_ name: StaticString) -> Span {
        Span(state: signposter.beginInterval(name))
    }

    /// End `span`, begun under the same `name`.
    public static func end(_ name: StaticString, _ span: Span) {
        signposter.endInterval(name, span.state)
    }
    #else
    /// Linux: no `os`, so intervals are no-ops.
    public static func begin(_ name: StaticString) -> Span {
        Span()
    }

    public static func end(_ name: StaticString, _ span: Span) {}
    #endif
}
