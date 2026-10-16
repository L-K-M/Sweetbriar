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
///   text, grown over any span they would cut and any span of the
///   style they would touch, so markers stay nested and never run
///   into each other; spans of the style inside dissolve into the new
///   one.
///
/// A caret inside a span of the style, markers included, unwraps it;
/// any other caret gets a marker pair with the caret between. Markers
/// never cross a paragraph — the styler reads one at a time — so a
/// selection over several is formatted paragraph by paragraph.
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
        /// what it selected plus the stretch the command rewrote
        /// around it, so the same command again toggles back.
        let selections: [NSRange]
    }

    /// The edit that toggles `marker` over `selections` in `text`.
    static func edit(
        for marker: Marker,
        in text: String,
        selections: [NSRange]
    ) -> Edit {
        let reader = Reader(text as NSString)
        let snapped = selections
            .map { reader.snap($0) }
            .sorted { $0.location < $1.location }
        let ranges = snapped.filter { $0.length > 0 }

        var plan = Plan(marker: marker)
        if ranges.isEmpty {
            for caret in snapped {
                plan.toggle(caret: caret.location, in: reader.paragraph(at: caret.location))
            }
        } else {
            let pieces = ranges
                .flatMap { reader.pieces(of: $0) }
                .filter { !$0.content.isEmpty }
            if pieces.allSatisfy({ $0.isStyled(marker.style) }) {
                plan.unwrap(pieces)
            } else {
                plan.wrap(pieces)
            }
        }

        let replacements = plan.replacements(in: reader.source)
        let after = snapped.map { plan.selection(after: $0, through: replacements) }
        return Edit(replacements: replacements, selections: merged(after))
    }

    // MARK: reading the source

    /// A strong or emphasis span, in document coordinates.
    private struct Span {
        let range: NSRange
        let style: MarkdownStyler.Style

        /// Its two markers: two characters each side for strong, one
        /// for emphasis.
        var markers: [NSRange] {
            let m = style == .strong ? 2 : 1
            return [
                NSRange(location: range.location, length: m),
                NSRange(location: NSMaxRange(range) - m, length: m),
            ]
        }
    }

    /// One paragraph as the styler reads it.
    private struct Paragraph {
        let range: NSRange
        /// The strong and emphasis spans, outermost first.
        let spans: [Span]
        /// Per position, paragraph-relative: syntax, inside a strong
        /// span, inside an emphasis span — lookups, not scans of the
        /// spans per character. (Foundation's `IndexSet` unions are
        /// quadratic on Linux.)
        private let marker: [Bool]
        private let strong: [Bool]
        private let emphasis: [Bool]

        init(range: NSRange, source: NSString) {
            var spans: [Span] = []
            var marker = [Bool](repeating: false, count: range.length)
            var strong = marker
            var emphasis = marker
            for run in MarkdownStyler.styles(forParagraph: source.substring(with: range)) {
                let positions = run.range.location ..< NSMaxRange(run.range)
                switch run.style {
                case .strong:
                    positions.forEach { strong[$0] = true }
                case .emphasis:
                    positions.forEach { emphasis[$0] = true }
                case .marker:
                    positions.forEach { marker[$0] = true }
                case .heading, .listItem:
                    continue
                }
                if run.style != .marker {
                    let shifted = NSRange(
                        location: run.range.location + range.location,
                        length: run.range.length
                    )
                    spans.append(Span(range: shifted, style: run.style))
                }
            }
            self.range = range
            self.spans = spans
            self.marker = marker
            self.strong = strong
            self.emphasis = emphasis
        }

        /// Whether the styler marks `position` as syntax.
        func isMarker(_ position: Int) -> Bool {
            marker[position - range.location]
        }

        /// Whether `position` sits in a span of `style`.
        func isCovered(_ position: Int, by style: MarkdownStyler.Style) -> Bool {
            let covered = style == .strong ? strong : emphasis
            return covered[position - range.location]
        }
    }

    /// A selection's part inside one paragraph.
    private struct Piece {
        let paragraph: Paragraph
        /// The text it holds — composed characters that are neither
        /// syntax nor led by whitespace — ascending.
        let content: [NSRange]

        /// Whether every text character sits in a span of `style`.
        func isStyled(_ style: MarkdownStyler.Style) -> Bool {
            content.allSatisfy { paragraph.isCovered($0.location, by: style) }
        }

        /// Whether a text character starts inside `range` — a binary
        /// search, since `content` ascends.
        func hasContent(in range: NSRange) -> Bool {
            var low = 0
            var high = content.count
            while low < high {
                let mid = (low + high) / 2
                if content[mid].location < range.location {
                    low = mid + 1
                } else {
                    high = mid
                }
            }
            return low < content.count && content[low].location < NSMaxRange(range)
        }

        /// From its first to its last text character.
        var window: NSRange {
            guard let first = content.first, let last = content.last else {
                return NSRange(location: 0, length: 0)
            }
            return NSRange(location: first.location, length: NSMaxRange(last) - first.location)
        }
    }

    /// The source and its paragraphs, each read by the styler once.
    private final class Reader {
        let source: NSString
        private var paragraphs: [Int: Paragraph] = [:]

        init(_ source: NSString) {
            self.source = source
        }

        /// `selection` clamped to the text and widened to whole
        /// composed characters, so markers never split a surrogate
        /// pair or a combining sequence.
        func snap(_ selection: NSRange) -> NSRange {
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

        /// The paragraph holding `location`.
        func paragraph(at location: Int) -> Paragraph {
            let range = source.paragraphRange(for: NSRange(location: location, length: 0))
            if let cached = paragraphs[range.location] {
                return cached
            }
            let paragraph = Paragraph(range: range, source: source)
            paragraphs[range.location] = paragraph
            return paragraph
        }

        /// `selection` cut at paragraph boundaries.
        func pieces(of selection: NSRange) -> [Piece] {
            var pieces: [Piece] = []
            var location = selection.location
            while location < NSMaxRange(selection) {
                let paragraph = paragraph(at: location)
                let end = min(NSMaxRange(selection), NSMaxRange(paragraph.range))
                var content: [NSRange] = []
                while location < end {
                    let character = source.rangeOfComposedCharacterSequence(at: location)
                    if !paragraph.isMarker(character.location),
                       !isWhitespace(source.character(at: character.location)) {
                        content.append(character)
                    }
                    location = max(location + 1, NSMaxRange(character))
                }
                pieces.append(Piece(paragraph: paragraph, content: content))
                location = max(location, NSMaxRange(paragraph.range))
            }
            return pieces
        }

        private func isWhitespace(_ unit: unichar) -> Bool {
            guard let scalar = Unicode.Scalar(unit) else { return false }
            return CharacterSet.whitespacesAndNewlines.contains(scalar)
        }
    }

    // MARK: planning the edit

    /// The command's changes before they become replacements: marker
    /// characters to delete, markers to insert, and the stretches
    /// rewritten — a selection touching one grows over it.
    private struct Plan {
        let marker: Marker
        /// Marker stretches to delete, possibly overlapping.
        private var deletions: [NSRange] = []
        private var insertions: [(position: Int, text: String)] = []
        private var regions: [NSRange] = []
        /// Carets given a marker pair: they land between the markers.
        private var pairedCarets = Set<Int>()
        /// Spans already unwrapped, by start — once is enough.
        private var unwrapped = Set<Int>()

        init(marker: Marker) {
            self.marker = marker
        }

        /// Unwrapping: each span of the style holding a selected text
        /// character loses its markers.
        mutating func unwrap(_ pieces: [Piece]) {
            for piece in pieces {
                for span in piece.paragraph.spans where span.style == marker.style
                    && piece.hasContent(in: span.range) {
                    unwrap(span)
                }
            }
        }

        /// Wrapping: each piece without the style is wrapped from its
        /// first to its last text character, grown over the spans the
        /// markers would cut or touch; spans of the style inside
        /// dissolve. Pieces of a paragraph that end up meeting wrap
        /// together.
        mutating func wrap(_ pieces: [Piece]) {
            // Selections arrive sorted, so a paragraph's pieces are
            // adjacent.
            var groups: [[Piece]] = []
            for piece in pieces where !piece.isStyled(marker.style) {
                if let last = groups.last?.last, last.paragraph.range == piece.paragraph.range {
                    groups[groups.count - 1].append(piece)
                } else {
                    groups.append([piece])
                }
            }

            for group in groups {
                guard let spans = group.first?.paragraph.spans else { continue }
                for window in settled(group.map(\.window), spans: spans) {
                    for span in spans where span.style == marker.style
                        && MarkdownFormatting.contains(window, span.range) {
                        deletions += span.markers
                    }
                    insertions.append((window.location, marker.text))
                    insertions.append((NSMaxRange(window), marker.text))
                    regions.append(window)
                }
            }
        }

        /// A caret inside a span of the style unwraps the innermost
        /// one; elsewhere it gets a marker pair.
        mutating func toggle(caret: Int, in paragraph: Paragraph) {
            let holding = paragraph.spans.last {
                $0.style == marker.style
                    && $0.range.location <= caret && caret <= NSMaxRange($0.range)
            }
            if let holding {
                unwrap(holding)
                return
            }
            insertions.append((caret, marker.text + marker.text))
            pairedCarets.insert(caret)
        }

        private mutating func unwrap(_ span: Span) {
            guard unwrapped.insert(span.range.location).inserted else { return }
            deletions += span.markers
            regions.append(span.range)
        }

        /// `windows` grown over every span crossing their edges and
        /// every span of the style touching them, then joined where
        /// they meet, until neither changes anything.
        private func settled(_ windows: [NSRange], spans: [Span]) -> [NSRange] {
            var windows = windows
            while true {
                let grown = windows.map { window in
                    var window = window
                    var changed = true
                    while changed {
                        changed = false
                        for span in spans where grows(window, over: span) {
                            window = NSUnionRange(window, span.range)
                            changed = true
                        }
                    }
                    return window
                }
                let next = joined(grown)
                if next == windows {
                    return next
                }
                windows = next
            }
        }

        /// Whether `window` must take in `span`: markers at its edges
        /// would cut the span, or run into the span's own markers.
        private func grows(_ window: NSRange, over span: Span) -> Bool {
            let overlap = NSIntersectionRange(window, span.range).length > 0
            if overlap {
                return !MarkdownFormatting.contains(window, span.range)
                    && !MarkdownFormatting.contains(span.range, window)
            }
            let touches = NSMaxRange(span.range) == window.location
                || NSMaxRange(window) == span.range.location
            return touches && span.style == marker.style
        }

        /// `ranges` sorted, overlapping or touching ones joined.
        private func joined(_ ranges: [NSRange]) -> [NSRange] {
            var result: [NSRange] = []
            for range in ranges.sorted(by: { $0.location < $1.location }) {
                guard let last = result.last, range.location <= NSMaxRange(last) else {
                    result.append(range)
                    continue
                }
                result[result.count - 1] = NSUnionRange(last, range)
            }
            return result
        }

        /// The plan as replacements: each deleted stretch and each
        /// insertion, joined where they meet — an insertion beside a
        /// deletion becomes one replacement — and dropped where the
        /// text stays the same.
        func replacements(in source: NSString) -> [Replacement] {
            var pieces = joined(deletions).map { Replacement(range: $0, string: "") }
            pieces += insertions.map {
                Replacement(range: NSRange(location: $0.position, length: 0), string: $0.text)
            }
            pieces.sort {
                ($0.range.location, $0.range.length) < ($1.range.location, $1.range.length)
            }

            var result: [Replacement] = []
            for piece in pieces {
                guard let last = result.last,
                      NSMaxRange(last.range) == piece.range.location else {
                    result.append(piece)
                    continue
                }
                result[result.count - 1] = Replacement(
                    range: NSUnionRange(last.range, piece.range),
                    string: last.string + piece.string
                )
            }
            return result.filter { $0.string != source.substring(with: $0.range) }
        }

        /// Where `selection` lands: a range grows over the stretches
        /// rewritten around it; a caret given a pair sits between the
        /// markers.
        func selection(after selection: NSRange, through replacements: [Replacement]) -> NSRange {
            guard selection.length > 0 else {
                let caret = translate(selection.location, through: replacements, edge: .start)
                let offset = pairedCarets.contains(selection.location)
                    ? marker.text.utf16.count
                    : 0
                return NSRange(location: caret + offset, length: 0)
            }
            let affected = regions
                .filter { NSIntersectionRange($0, selection).length > 0 }
                .reduce(selection) { NSUnionRange($0, $1) }
            let start = translate(affected.location, through: replacements, edge: .start)
            let end = translate(NSMaxRange(affected), through: replacements, edge: .end)
            return NSRange(location: start, length: end - start)
        }
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
