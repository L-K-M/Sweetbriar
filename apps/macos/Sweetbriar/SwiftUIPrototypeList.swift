// The SwiftUI prototype list (plan M0.10): a `List` over the same
// `ListWindowModel`, guarded as the plan requires — `ForEach` over all
// ids with stable occurrence-id identity, one constant row view type,
// rows observed one by one through their `Page`, no `AnyView`, no
// conditional row types. Hosted in an `NSHostingView`; the adapter
// below carries the `PrototypeList` seam.

import AppKit
import SwiftUI
import SweetbriarBridge

/// The scroll metrics the list reports per geometry change: the
/// content-space offset of the viewport's top edge and the viewport's
/// height. Equatable so unchanged values don't fire `action`.
private struct ScrollMetrics: Equatable {
    var top: CGFloat
    var height: CGFloat
}

/// What the `List` body mutates between evaluations and the
/// `PrototypeList` adapter reads back — selection by occurrence id,
/// the normalized scroll offset and the `ScrollViewProxy` the harness
/// scrolls through.
@MainActor
@Observable
private final class PrototypeListState {
    /// The selection as occurrence ids — `Set<Int64>` like AppKit's.
    var selectedIDs: Set<Int64> = []

    /// Content-space `y` of the viewport's top edge: the observed
    /// `contentOffset.y` plus the top content inset, so a row's
    /// `rank * pitch` lands in the same space.
    var scrollTop: CGFloat = 0

    /// The viewport's height in points, for `visibleRanks`.
    var containerHeight: CGFloat = 0

    /// The reader's proxy, captured on appear; `scrollToRank` uses it.
    var scrollProxy: ScrollViewProxy?
}

/// The `NSView` half of the SwiftUI list: hosts the root view in an
/// `NSHostingView` and answers the `PrototypeList` seam from the
/// observed scroll state — never from the model.
@MainActor
final class SwiftUIPrototypeList: NSView {
    /// The account palette; same colors, same order as the AppKit
    /// list's so `accountColorIndex` matches.
    fileprivate static let accountPalette: [Color] = [
        .blue, .green, .orange, .purple, .pink, .teal,
    ]

    private let model: ListWindowModel
    private let state: PrototypeListState
    private let hosting: NSHostingView<PrototypeListRoot>

    init(
        model: ListWindowModel,
        formatter: MessageRowFormatter,
        onIndexChange: (@MainActor () -> Void)? = nil
    ) {
        self.model = model
        // Locals: `self` can't be read until `hosting` is set.
        let state = PrototypeListState()
        self.state = state
        self.hosting = NSHostingView(
            rootView: PrototypeListRoot(
                state: state,
                model: model,
                formatter: formatter
            )
        )
        super.init(frame: .zero)
        hosting.autoresizingMask = [.width, .height]
        hosting.frame = bounds
        addSubview(hosting)

        // The body tracks `ids` itself; the hook re-pushes the range
        // for the new row count, and refreshes the window's
        // empty-state hint when a fill lands.
        model.onIndexChanged = { [weak self] in
            self?.pushVisibleRange()
            onIndexChange?()
        }
    }

    /// The list may report no scroll geometry before the first
    /// scroll: seed the viewport height from the host, so the first
    /// rows are read at once.
    override func layout() {
        super.layout()
        guard state.containerHeight == 0, bounds.height > 0 else { return }
        state.containerHeight = bounds.height
        pushVisibleRange()
    }

    /// The visible ranks into the model, from the observed scroll
    /// state.
    private func pushVisibleRange() {
        model.setVisible(visibleRanks)
    }

    required init?(coder: NSCoder) {
        fatalError("SwiftUIPrototypeList is built in code")
    }
}

extension SwiftUIPrototypeList: PrototypeList {
    /// `rank` to the viewport's top through `ScrollViewReader` — the
    /// rank's occurrence id comes straight from the id index.
    func scrollToRank(_ rank: Int) {
        guard model.count > 0 else { return }
        let clamped = min(max(0, rank), model.count - 1)
        guard let id = model.occurrenceID(at: clamped) else { return }
        state.scrollProxy?.scrollTo(id, anchor: .top)
    }

    /// From the observed scroll position: `top` is content-space, so
    /// ranks are `top / pitch` … `(top + height) / pitch`, clamped to
    /// the row range — the same conversion AppKit's `rows(in:)` makes.
    var visibleRanks: Range<Int> {
        MessageRowMetrics.visibleRanks(
            top: state.scrollTop,
            height: state.containerHeight,
            count: model.count
        )
    }

    /// `rank`'s rect in window coordinates: `rank * pitch` less the
    /// scroll offset is the row's top in the hosting view's space;
    /// honor the view's flippedness, then convert to the window.
    func rowRect(forRank rank: Int) -> NSRect? {
        guard rank >= 0, rank < model.count else { return nil }
        let top = CGFloat(rank) * MessageRowMetrics.pitch - state.scrollTop
        let y = hosting.isFlipped
            ? top : hosting.bounds.height - top - MessageRowMetrics.pitch
        let rect = NSRect(
            x: 0, y: y,
            width: hosting.bounds.width, height: MessageRowMetrics.pitch
        )
        return hosting.convert(rect, to: nil)
    }

    var selectedIDs: Set<Int64> {
        state.selectedIDs
    }

    var listView: NSView {
        hosting
    }
}

/// The list itself. `ForEach` reads `model.ids` through `RankedIDs`,
/// so a replaced index rebuilds the diff — and each row watches only
/// its own `Page`, so a landing page redraws just its rows (spec/08).
/// Marked `@MainActor` outright: the modern SDK already infers it
/// from `View`, and older semantics isolate only `body`.
@MainActor
private struct PrototypeListRoot: View {
    @Bindable var state: PrototypeListState
    let model: ListWindowModel
    let formatter: MessageRowFormatter

    var body: some View {
        ScrollViewReader { proxy in
            List(selection: $state.selectedIDs) {
                // Selection tags come from `ForEach`'s own ids (Int64,
                // like the selection set): no per-row `.tag`. The row
                // traits are uniform, so they sit on the `ForEach`,
                // outside the element builder.
                ForEach(RankedIDs(model.ids)) { element in
                    MessageRowView(
                        page: model.page(containing: element.rank),
                        offset: element.rank % ListWindowModel.pageSize,
                        formatter: formatter
                    )
                    // Exactly `pitch` apart: fixed row height, no
                    // insets, no separators, and the list's minimum
                    // row height capped at the pitch.
                    .frame(height: MessageRowMetrics.pitch)
                }
                .listRowInsets(EdgeInsets())
                .listRowSeparator(.hidden)
            }
            .listStyle(.plain)
            .environment(
                \.defaultMinListRowHeight, MessageRowMetrics.pitch
            )
            .onScrollGeometryChange(
                for: ScrollMetrics.self,
                of: { geometry in
                    // `visibleRect` is the viewport in content space —
                    // the same rect AppKit's `rows(in:)` reads.
                    ScrollMetrics(
                        top: geometry.visibleRect.minY,
                        height: geometry.visibleRect.height
                    )
                },
                action: { _, new in
                    MainActor.assumeIsolated {
                        scrollDidChange(new)
                    }
                }
            )
            .contextMenu(forSelectionType: Int64.self) { targets in
                // `forSelectionType` already applies spec/08's rule:
                // `targets` is the selection for a click on a
                // selected row, the clicked row alone otherwise. An
                // empty set is a click below the rows — show nothing,
                // like AppKit's `clickedRow == -1`.
                if !targets.isEmpty {
                    Button(ContextClickTarget.targetSummary(count: targets.count)) {}
                        .disabled(true)
                    Divider()
                    ForEach(ContextClickTarget.actionTitles, id: \.self) {
                        title in
                        Button(title) {}
                    }
                }
            }
            .onAppear {
                MainActor.assumeIsolated {
                    state.scrollProxy = proxy
                    pushVisibleRange()
                }
            }
            // A rebuild swaps the whole index; re-push the visible
            // range so the fresh window's pages get read.
            .onChange(of: model.generation) { _, _ in
                MainActor.assumeIsolated {
                    pushVisibleRange()
                }
            }
        }
    }

    /// The ranks the viewport shows right now, into the model — the
    /// only fetch driver (spec/08); no per-row `onAppear`.
    @MainActor
    private func scrollDidChange(_ metrics: ScrollMetrics) {
        state.scrollTop = metrics.top
        state.containerHeight = metrics.height
        pushVisibleRange()
    }

    @MainActor
    private func pushVisibleRange() {
        model.setVisible(
            MessageRowMetrics.visibleRanks(
                top: state.scrollTop,
                height: state.containerHeight,
                count: model.count
            )
        )
    }
}

/// The one constant row view type (plan M0.10): a populated row, or
/// the placeholder while its page is out — same height, no text, a
/// subtle fill. It observes only its `Page`, so a landing page
/// invalidates only that page's rows.
@MainActor
private struct MessageRowView: View {
    let page: ListWindowModel.Page
    let offset: Int
    let formatter: MessageRowFormatter

    var body: some View {
        if let row = page.row(at: offset) {
            populated(formatter.present(row))
        } else {
            placeholder
        }
    }

    /// Same content, same order as the AppKit cell: rail + monogram,
    /// sender (bold when unread) with date trailing, subject, one-line
    /// snippet, and the markers.
    private func populated(_ content: MessageRowContent) -> some View {
        let accountColor = Self.accountColor(at: content.accountColorIndex)
        return HStack(spacing: 0) {
            Rectangle()
                .fill(accountColor)
                .frame(width: MessageRowMetrics.railWidth)
            ZStack {
                Circle().fill(accountColor)
                Text(content.monogram)
                    .font(.system(size: 11, weight: .semibold))
                    .foregroundStyle(.white)
            }
            .frame(
                width: MessageRowMetrics.monogramSize,
                height: MessageRowMetrics.monogramSize
            )
            .padding(.leading, MessageRowMetrics.innerGap)
            VStack(alignment: .leading, spacing: 3) {
                HStack(spacing: MessageRowMetrics.markerSpacing) {
                    if content.unread {
                        Circle()
                            .fill(Color.accentColor)
                            .frame(
                                width: MessageRowMetrics.unreadDotSize,
                                height: MessageRowMetrics.unreadDotSize
                            )
                    }
                    Text(content.sender)
                        .font(.system(
                            size: 13,
                            weight: content.unread ? .bold : .regular
                        ))
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    if content.flagged {
                        Image(systemName: "flag.fill")
                            .font(.system(size: 10))
                            .foregroundStyle(.orange)
                    }
                    if content.hasAttachments {
                        Image(systemName: "paperclip")
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                    }
                    Text(content.date)
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                }
                Text(content.subject)
                    .font(.system(size: 13))
                    .lineLimit(1)
                HStack(spacing: MessageRowMetrics.markerSpacing) {
                    Text(content.snippet)
                        .font(.system(size: 12))
                        .foregroundStyle(.secondary)
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    ForEach(content.tags, id: \.self) { tag in
                        Text(tag)
                            .font(.system(size: 10))
                            .foregroundStyle(.secondary)
                            .padding(.horizontal, MessageRowMetrics.tagChipPadding)
                            .padding(.vertical, 1)
                            .background(
                                Capsule().fill(Color.secondary.opacity(0.15))
                            )
                    }
                }
            }
            // No vertical padding: the three lines total ~55 pt of the
            // 56-pt pitch — padding would push the snippet past the
            // `.frame(height:)` clip.
            .padding(.leading, MessageRowMetrics.innerGap)
            .padding(.trailing, MessageRowMetrics.trailingPadding)
        }
        .clipped()
        // One element, one label — the shared VoiceOver order
        // (sender, subject, date, unread, flagged).
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(verbatim: content.accessibilityLabel))
    }

    private var placeholder: some View {
        RoundedRectangle(
            cornerRadius: MessageRowMetrics.placeholderCornerRadius
        )
        .fill(
            Color.primary.opacity(MessageRowMetrics.placeholderFillOpacity)
        )
        .padding(.horizontal, MessageRowMetrics.placeholderInsetX)
        .padding(.vertical, MessageRowMetrics.placeholderInsetY)
    }

    /// The palette slot's color — the same slot the AppKit cell uses.
    private static func accountColor(at index: Int) -> Color {
        SwiftUIPrototypeList.accountPalette[
            index % SwiftUIPrototypeList.accountPalette.count
        ]
    }
}
