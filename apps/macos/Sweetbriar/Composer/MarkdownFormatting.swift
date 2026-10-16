// The composer's ⌘B/⌘I transformation (plan M0.11, spec/08
// "Composer"): formatting commands edit the Markdown source itself —
// add `**`/`*` markers, or remove the ones already there — as one
// undoable edit computed here. It reads the source through
// `MarkdownStyler`, so "already bold" means what the display shows.
// Pure and Foundation-only so it tests on Linux.

import Foundation

/// Toggle edits for the composer's formatting commands.
///
/// A command toggles one style over every selection at once:
/// - every selected text character (not syntax, not whitespace)
///   already has the style → each span giving it loses its markers,
///   the whole span — Markdown can't unstyle part of one without
///   splitting it;
/// - otherwise each selection gains the style: markers go around its
///   text, grown over any span they would cut so markers stay nested,
///   and spans of the same style inside it dissolve into the new one.
///
/// Markers never cross a paragraph — the styler reads one at a time —
/// so a selection over several is formatted paragraph by paragraph.
/// A caret inserts a marker pair with the caret between.
enum MarkdownFormatting {
    /// Which style the command toggles.
    enum Marker {
        /// `**…**` — ⌘B.
        case strong
        /// `*…*` — ⌘I.
        case emphasis

        /// The source characters inserted.
        var text: String {
            switch self {
            case .strong:
                return "**"
            case .emphasis:
                return "*"
            }
        }

        /// The styler's style for this marker's spans.
        fileprivate var style: MarkdownStyler.Style {
            switch self {
            case .strong:
                return .strong
            case .emphasis:
                return .emphasis
            }
        }
    }

    /// One text replacement.
    struct Replacement: Equatable {
        /// The range replaced, in the pre-edit string's UTF-16 units.
        let range: NSRange
        /// What replaces it.
        let string: String
    }

    /// A command's whole effect, applied as one undoable edit.
    struct Edit: Equatable {
        /// Ascending and disjoint, in pre-edit coordinates — applied
        /// last to first, each leaves the earlier ranges intact.
        let replacements: [Replacement]
        /// The selections after, in post-edit coordinates: each covers
        /// what it selected plus the markers the command touched, so
        /// the same command again toggles back.
        let selections: [NSRange]
    }

    /// The edit that toggles `marker` over `selections` in `text`.
    static func edit(
        for marker: Marker,
        in text: String,
        selections: [NSRange]
    ) -> Edit {
        let source = text as NSString
        let snapped = selections.map { snap($0, in: source) }
        let ranges = snapped.filter { $0.length > 0 }

        // NSTextView allows a caret only as the sole selection.
        guard !ranges.isEmpty else {
            guard let caret = snapped.first else {
                return Edit(replacements: [], selections: [])
            }
            let m = marker.text.utf16.count
            return Edit(
                replacements: [Replacement(
                    range: NSRange(location: caret.location, length: 0),
                    string: marker.text + marker.text
                )],
                selections: [NSRange(location: caret.location + m, length: 0)]
            )
        }

        let pieces = ranges
            .flatMap { paragraphPieces(of: $0, in: source) }
            .filter { !$0.content.isEmpty }
        guard !pieces.isEmpty else {
            return Edit(replacements: [], selections: ranges)
        }

        let regions = pieces.allSatisfy { $0.isStyled(marker.style) }
            ? unwrapRegions(pieces, style: marker.style)
            : wrapRegions(pieces, style: marker.style)
        let replacements = regions
            .map { $0.replacement(in: source, marker: marker) }
            .sorted { $0.range.location < $1.range.location }

        let after = ranges.map { selection in
            let affected = regions
                .filter { NSIntersectionRange($0.range, selection).length > 0 }
                .reduce(selection) { NSUnionRange($0, $1.range) }
            let start = translate(affected.location, through: replacements, edge: .start)
            let end = translate(NSMaxRange(affected), through: replacements, edge: .end)
            return NSRange(location: start, length: end - start)
        }
        return Edit(replacements: replacements, selections: merged(after))
    }

    // MARK: reading the source

    /// One paragraph and the styler's spans in it, in document
    /// coordinates.
    private struct Paragraph {
        let range: NSRange
        /// The strong and emphasis spans, markers included.
        let spans: [Span]
        /// Positions the styler marks as syntax.
        let markers: IndexSet
    }

    /// A strong or emphasis span.
    private struct Span: Equatable {
        let range: NSRange
        let style: MarkdownStyler.Style

        /// Its two markers' positions: two characters each side for
        /// strong, one for emphasis.
        var markerPositions: IndexSet {
            let m = style == .strong ? 2 : 1
            var positions = IndexSet(integersIn: range.location ..< range.location + m)
            positions.insert(integersIn: NSMaxRange(range) - m ..< NSMaxRange(range))
            return positions
        }
    }

    /// A selection's part inside one paragraph.
    private struct Piece {
        let paragraph: Paragraph
        let range: NSRange
        /// The positions in `range` holding text — not syntax, not
        /// whitespace — ascending.
        let content: [Int]

        /// Whether every text character sits in a span of `style`.
        func isStyled(_ style: MarkdownStyler.Style) -> Bool {
            content.allSatisfy { position in
                paragraph.spans.contains {
                    $0.style == style && NSLocationInRange(position, $0.range)
                }
            }
        }
    }

    /// `selection` clamped to the text and widened to whole composed
    /// characters, so markers never split a surrogate pair or a
    /// combining sequence.
    private static func snap(_ selection: NSRange, in source: NSString) -> NSRange {
        let location = min(max(selection.location, 0), source.length)
        let length = min(max(selection.length, 0), source.length - location)
        guard location < source.length else {
            return NSRange(location: location, length: 0)
        }
        guard length > 0 else {
            let character = source.rangeOfComposedCharacterSequence(at: location)
            return NSRange(location: character.location, length: 0)
        }
        return source.rangeOfComposedCharacterSequences(
            for: NSRange(location: location, length: length)
        )
    }

    /// `selection` cut at paragraph boundaries, each part with its
    /// paragraph's styling.
    private static func paragraphPieces(of selection: NSRange, in source: NSString) -> [Piece] {
        var pieces: [Piece] = []
        var location = selection.location
        while location < NSMaxRange(selection) {
            let paragraph = read(
                source.paragraphRange(for: NSRange(location: location, length: 0)),
                in: source
            )
            let range = NSIntersectionRange(selection, paragraph.range)
            let content = (range.location ..< NSMaxRange(range)).filter {
                !paragraph.markers.contains($0)
                    && !isWhitespace(source.character(at: $0))
            }
            pieces.append(Piece(paragraph: paragraph, range: range, content: content))
            location = max(location + 1, NSMaxRange(paragraph.range))
        }
        return pieces
    }

    /// The styler's reading of the paragraph at `range`.
    private static func read(_ range: NSRange, in source: NSString) -> Paragraph {
        var spans: [Span] = []
        var markers = IndexSet()
        for run in MarkdownStyler.styles(forParagraph: source.substring(with: range)) {
            let shifted = NSRange(
                location: run.range.location + range.location,
                length: run.range.length
            )
            switch run.style {
            case .strong, .emphasis:
                spans.append(Span(range: shifted, style: run.style))
            case .marker:
                markers.insert(integersIn: shifted.location ..< NSMaxRange(shifted))
            case .heading, .listItem:
                break
            }
        }
        return Paragraph(range: range, spans: spans, markers: markers)
    }

    private static func isWhitespace(_ unit: unichar) -> Bool {
        guard let scalar = Unicode.Scalar(unit) else { return false }
        return CharacterSet.whitespacesAndNewlines.contains(scalar)
    }

    // MARK: planning the edit

    /// A stretch of source rewritten: the markers inside it deleted,
    /// and — when it gains the style — markers added around it.
    private struct Region {
        enum Change {
            case wrap
            case unwrap
        }

        var range: NSRange
        var deletions: IndexSet
        let change: Change

        func replacement(in source: NSString, marker: Marker) -> Replacement {
            var kept = ""
            let positions = IndexSet(integersIn: range.location ..< NSMaxRange(range))
                .subtracting(deletions)
            for stretch in positions.rangeView {
                kept += source.substring(with: NSRange(
                    location: stretch.lowerBound,
                    length: stretch.count
                ))
            }
            switch change {
            case .wrap:
                return Replacement(range: range, string: marker.text + kept + marker.text)
            case .unwrap:
                return Replacement(range: range, string: kept)
            }
        }
    }

    /// Unwrapping: each `style` span holding a selected text
    /// character loses its markers. Overlapping spans — one nested
    /// in another — rewrite as one region.
    private static func unwrapRegions(
        _ pieces: [Piece],
        style: MarkdownStyler.Style
    ) -> [Region] {
        var spans: [Span] = []
        for piece in pieces {
            for span in piece.paragraph.spans where span.style == style
                && !spans.contains(span)
                && piece.content.contains(where: { NSLocationInRange($0, span.range) }) {
                spans.append(span)
            }
        }
        let regions = spans.map {
            Region(range: $0.range, deletions: $0.markerPositions, change: .unwrap)
        }
        return mergedRegions(regions)
    }

    /// Wrapping: each piece without the style is wrapped from its
    /// first to its last text character, grown over any span the
    /// markers would cut; `style` spans inside dissolve. Pieces of a
    /// paragraph that end up overlapping wrap together.
    private static func wrapRegions(
        _ pieces: [Piece],
        style: MarkdownStyler.Style
    ) -> [Region] {
        var regions: [Region] = []
        let unstyled = pieces.filter { !$0.isStyled(style) }
        let paragraphs = unstyled.reduce(into: [NSRange]()) { found, piece in
            if !found.contains(piece.paragraph.range) {
                found.append(piece.paragraph.range)
            }
        }
        for paragraphRange in paragraphs {
            let inParagraph = unstyled.filter { $0.paragraph.range == paragraphRange }
            guard let spans = inParagraph.first?.paragraph.spans else { continue }

            var windows = inParagraph.compactMap { piece -> NSRange? in
                guard let first = piece.content.first,
                      let last = piece.content.last else {
                    return nil
                }
                return NSRange(location: first, length: last + 1 - first)
            }
            windows = settled(windows, spans: spans)

            for window in windows {
                var deletions = IndexSet()
                for span in spans where span.style == style && contains(window, span.range) {
                    deletions.formUnion(span.markerPositions)
                }
                regions.append(Region(range: window, deletions: deletions, change: .wrap))
            }
        }
        return regions
    }

    /// `windows` grown over every span crossing their edges, and
    /// merged where they overlap, until neither changes anything.
    private static func settled(_ windows: [NSRange], spans: [Span]) -> [NSRange] {
        var windows = windows
        while true {
            let grown = windows.map { window in
                var window = window
                var changed = true
                while changed {
                    changed = false
                    for span in spans where crosses(span.range, window) {
                        window = NSUnionRange(window, span.range)
                        changed = true
                    }
                }
                return window
            }
            let next = merged(grown)
            if next == windows {
                return next
            }
            windows = next
        }
    }

    /// Whether `a` and `b` overlap with neither containing the other.
    private static func crosses(_ a: NSRange, _ b: NSRange) -> Bool {
        NSIntersectionRange(a, b).length > 0 && !contains(a, b) && !contains(b, a)
    }

    private static func contains(_ outer: NSRange, _ inner: NSRange) -> Bool {
        outer.location <= inner.location && NSMaxRange(inner) <= NSMaxRange(outer)
    }

    /// `ranges` sorted, overlapping ones joined; touching ones stay
    /// apart.
    private static func merged(_ ranges: [NSRange]) -> [NSRange] {
        var result: [NSRange] = []
        for range in ranges.sorted(by: { $0.location < $1.location }) {
            guard let last = result.last,
                  NSIntersectionRange(last, range).length > 0 else {
                result.append(range)
                continue
            }
            result[result.count - 1] = NSUnionRange(last, range)
        }
        return result
    }

    /// `regions` sorted, overlapping ones joined with their deletions.
    private static func mergedRegions(_ regions: [Region]) -> [Region] {
        var result: [Region] = []
        for region in regions.sorted(by: { $0.range.location < $1.range.location }) {
            guard let last = result.last,
                  NSIntersectionRange(last.range, region.range).length > 0 else {
                result.append(region)
                continue
            }
            result[result.count - 1].range = NSUnionRange(last.range, region.range)
            result[result.count - 1].deletions.formUnion(region.deletions)
        }
        return result
    }

    // MARK: selections after

    /// Which end of a selection a position is: a start stays before
    /// text inserted at it, an end moves past it.
    private enum Edge {
        case start
        case end
    }

    /// Where pre-edit `position` lands after `replacements`.
    private static func translate(
        _ position: Int,
        through replacements: [Replacement],
        edge: Edge
    ) -> Int {
        var shift = 0
        for replacement in replacements {
            let range = replacement.range
            let newLength = replacement.string.utf16.count
            let before = NSMaxRange(range) < position
                || (NSMaxRange(range) == position
                    && (edge == .end || range.location < position))
            if before {
                shift += newLength - range.length
                continue
            }
            // Inside a replacement: snap to its start or its end.
            if range.location < position {
                return range.location + shift + (edge == .end ? newLength : 0)
            }
            break
        }
        return position + shift
    }
}
