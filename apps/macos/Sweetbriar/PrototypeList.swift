// The seam part B2b's benchmark harness drives identically in both
// prototype lists (plan M0.10): scroll, visibility, geometry and
// selection read the same way whether the list is AppKit or SwiftUI.

import AppKit

/// One prototype list under test, as the view the window installs.
/// Both implementations conform so the benchmark harness can't tell
/// them apart; every member is read or driven on the main actor.
@MainActor
protocol PrototypeList: NSView {
    /// Put `rank` at the viewport's top through the list's own
    /// scroll-position API — the clip view's `scroll(to:)` plus
    /// `reflectScrolledClipView` for AppKit,
    /// `ScrollViewReader.scrollTo(_:anchor: .top)` for SwiftUI. Ranks
    /// past the end clamp to the last row.
    func scrollToRank(_ rank: Int)

    /// The ranks the viewport shows, derived from the scroll position
    /// — never from the model. Partial rows at both edges count.
    var visibleRanks: Range<Int> { get }

    /// `rank`'s rect in window coordinates, for posting mouse clicks
    /// at it; nil when `rank` is outside the row range.
    func rowRect(forRank rank: Int) -> NSRect?

    /// The selection as occurrence ids (spec/08: selection is held by
    /// `occurrence_id`, so a 10k-row selection fetches no rows).
    var selectedIDs: Set<Int64> { get }

    /// The view scroll-wheel events are posted to — the scroll view
    /// for AppKit, the hosting view for SwiftUI.
    var listView: NSView { get }

    /// The list or its window still needs layout or display — the
    /// harness waits this out before trusting `visibleRanks`.
    var hasPendingLayout: Bool { get }
}
