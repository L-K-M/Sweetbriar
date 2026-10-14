// A display-frame source (spec/07 "Threading"): results reach
// `@MainActor` observers at most once per frame. The bridge only knows
// this protocol; the app supplies the real clock on a display link.

/// Something that fires once per display frame.
///
/// Each `nextFrame()` call is an independent wait: it returns when the
/// next frame fires. Calls not awaited simply don't observe that
/// frame. An implementation may coalesce concurrent waits onto the
/// same frame, and is free to ignore callers that go away.
///
/// The AppKit implementation (a `CADisplayLink` via
/// `NSView.displayLink(target:selector:)`) lives in the app target so
/// this file — and the whole bridge — stays free of AppKit and
/// type-checks on Linux.
public protocol FrameClock: Sendable {
    /// Suspend until the next display frame fires.
    func nextFrame() async
}
