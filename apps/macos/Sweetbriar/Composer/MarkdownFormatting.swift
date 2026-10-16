// The composer's ⌘B/⌘I transformation (plan M0.11, spec/08
// "Composer"): formatting commands edit the Markdown source itself —
// add `**`/`*` markers, or remove the ones already there — as one
// undoable edit computed here. It reads the source through
// `MarkdownStyler`, so "already bold" means what the display shows,
// and it re-reads each result the same way before using it. Pure and
// Foundation-only so it tests on Linux.

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
/// Each paragraph's result is parsed again and must change only what
/// the command meant: every other style and every piece of syntax
/// stays as it was. When it doesn't, a safer form is tried — keeping
/// the spans inside, then markers around whole words (`hello**.**`
/// can't open; `**hello.**` can) — and a paragraph with no safe form
/// is left alone.
///
/// A caret works on words, as word processors do: between an empty
/// marker pair it takes the pair out; inside a word it toggles the
/// word; elsewhere it gets a marker pair to type into — unless the
/// pair would pair up with markers around it, when the word beside
/// it toggles instead. Carets whose edits would meet keep only the
/// first. Markers never cross a paragraph — the styler reads one at
/// a time — so a selection over several is formatted paragraph by
/// paragraph.
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
                plan.toggle(caret: caret.location, reader: reader)
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
        return Edit(
            replacements: replacements,
            selections: plan.selections(after: snapped, through: replacements)
        )
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

    /// What one paragraph's part of the command does, in document
    /// coordinates — enough to check the result against its intent.
    private struct ParagraphEdit {
        /// Marker stretches to delete, possibly overlapping.
        var deletions: [NSRange] = []
        /// Markers added around text: they must parse as syntax.
        var insertions: [(position: Int, text: String)] = []
        /// Empty pairs added at carets: literal as they stand, and
        /// syntax around a styled letter once one is typed between.
        var pairs: [(position: Int, text: String)] = []
        /// Windows the style is added over.
        var wrapped: [NSRange] = []
        /// Spans of the style whose markers go.
        var unwrapped: [NSRange] = []

        /// The stretches rewritten — a selection touching one grows
        /// over it.
        var regions: [NSRange] {
            wrapped + unwrapped
        }

        /// `self` and `other` together.
        func adding(_ other: ParagraphEdit) -> ParagraphEdit {
            var both = self
            both.deletions += other.deletions
            both.insertions += other.insertions
            both.pairs += other.pairs
            both.wrapped += other.wrapped
            both.unwrapped += other.unwrapped
            return both
        }
    }

    /// Whether a check reads an edit's pairs as they stand or with a
    /// letter typed between their markers.
    private enum Typing {
        case none
        case letter
    }

    /// One paragraph as the styler reads it.
    private struct Paragraph {
        let range: NSRange
        let text: NSString
        let units: [UInt16]
        /// The strong and emphasis spans, outermost first.
        let spans: [Span]
        /// Per position, paragraph-relative: syntax, inside a strong
        /// span, inside an emphasis span — lookups, not scans of the
        /// spans per character.
        private let marker: [Bool]
        private let strong: [Bool]
        private let emphasis: [Bool]

        init(text: String, location: Int) {
            let units = Array(text.utf16)
            var spans: [Span] = []
            var markers: [Range<Int>] = []
            var strong: [Range<Int>] = []
            var emphasis: [Range<Int>] = []
            for run in MarkdownStyler.styles(forParagraph: text) {
                let positions = run.range.location ..< NSMaxRange(run.range)
                switch run.style {
                case .strong:
                    strong.append(positions)
                case .emphasis:
                    emphasis.append(positions)
                case .marker:
                    markers.append(positions)
                case .heading, .listItem:
                    continue
                }
                if run.style != .marker {
                    let shifted = NSRange(
                        location: run.range.location + location,
                        length: run.range.length
                    )
                    spans.append(Span(range: shifted, style: run.style))
                }
            }
            self.range = NSRange(location: location, length: units.count)
            self.text = text as NSString
            self.units = units
            self.spans = spans
            self.marker = Self.mask(markers, count: units.count)
            self.strong = Self.mask(strong, count: units.count)
            self.emphasis = Self.mask(emphasis, count: units.count)
        }

        /// Positions inside any of `ranges` — one sweep over boundary
        /// counts, so nested spans cost no more than flat ones.
        static func mask(_ ranges: [Range<Int>], count: Int) -> [Bool] {
            var delta = [Int](repeating: 0, count: count + 1)
            for range in ranges {
                delta[range.lowerBound] += 1
                delta[range.upperBound] -= 1
            }
            var depth = 0
            return (0 ..< count).map {
                depth += delta[$0]
                return depth > 0
            }
        }

        /// Whether the styler marks `position` as syntax.
        func isMarker(_ position: Int) -> Bool {
            marker[position - range.location]
        }

        /// Whether `position` sits in a span of `style`.
        func isCovered(_ position: Int, by style: MarkdownStyler.Style) -> Bool {
            covered(by: style)[position - range.location]
        }

        private func covered(by style: MarkdownStyler.Style) -> [Bool] {
            style == .strong ? strong : emphasis
        }

        // MARK: planning

        /// The wrap of `windows`: grown, then — for `.words` —
        /// widened to whole words first; spans of the style inside
        /// dissolve or stay.
        func wrapping(
            _ windows: [NSRange],
            marker: Marker,
            reach: Reach,
            inner: InnerSpans
        ) -> ParagraphEdit {
            let start = reach == .words ? windows.map(widenedToWords) : windows
            var edit = ParagraphEdit()
            for window in settled(start, style: marker.style) {
                if inner == .dissolve {
                    for span in spans where span.style == marker.style
                        && MarkdownFormatting.contains(window, span.range) {
                        edit.deletions += span.markers
                    }
                }
                edit.insertions.append((window.location, marker.text))
                edit.insertions.append((NSMaxRange(window), marker.text))
                edit.wrapped.append(window)
            }
            return edit
        }

        /// `window` out to the whitespace around it, a whole composed
        /// character at a time — a combining mark on a space stays
        /// with the space.
        func widenedToWords(_ window: NSRange) -> NSRange {
            var start = window.location - range.location
            while start > 0 {
                let character = text.rangeOfComposedCharacterSequence(at: start - 1)
                guard !isWhitespace(units[character.location]) else { break }
                start = character.location
            }
            var end = NSMaxRange(window) - range.location
            while end < units.count {
                let character = text.rangeOfComposedCharacterSequence(at: end)
                guard !isWhitespace(units[character.location]) else { break }
                end = NSMaxRange(character)
            }
            return NSRange(location: start + range.location, length: end - start)
        }

        /// Whether text, not whitespace or the paragraph's edge, sits
        /// on both sides of `caret`.
        func isInsideWord(_ caret: Int) -> Bool {
            let at = caret - range.location
            return at > 0 && at < units.count
                && !isWhitespace(units[at - 1]) && !isWhitespace(units[at])
        }

        /// The word ending or starting at `caret`, left first.
        func word(beside caret: Int) -> NSRange? {
            let at = caret - range.location
            if at > 0, !isWhitespace(units[at - 1]) {
                return widenedToWords(NSRange(location: caret - 1, length: 1))
            }
            if at < units.count, !isWhitespace(units[at]) {
                return widenedToWords(NSRange(location: caret, length: 1))
            }
            return nil
        }

        /// `windows` grown over every span crossing their edges and
        /// every span of `style` touching them, then joined where they
        /// meet, until nothing changes. Each edge grows in one pass:
        /// spans by descending start for the left edge, by ascending
        /// end for the right, so a chain of touching spans is taken in
        /// one sweep, not one span per scan.
        private func settled(_ windows: [NSRange], style: MarkdownStyler.Style) -> [NSRange] {
            let byStart = spans.sorted { $0.range.location > $1.range.location }
            let byEnd = spans.sorted { NSMaxRange($0.range) < NSMaxRange($1.range) }
            var windows = windows
            while true {
                let grown = windows.map { window in
                    var window = window
                    for span in byStart where span.range.location < window.location
                        && grows(window, over: span, style: style) {
                        window = NSUnionRange(window, span.range)
                    }
                    for span in byEnd where NSMaxRange(span.range) > NSMaxRange(window)
                        && grows(window, over: span, style: style) {
                        window = NSUnionRange(window, span.range)
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
        private func grows(
            _ window: NSRange,
            over span: Span,
            style: MarkdownStyler.Style
        ) -> Bool {
            let overlap = NSIntersectionRange(window, span.range).length > 0
            if overlap {
                return !MarkdownFormatting.contains(window, span.range)
                    && !MarkdownFormatting.contains(span.range, window)
            }
            let touches = NSMaxRange(span.range) == window.location
                || NSMaxRange(window) == span.range.location
            return touches && span.style == style
        }

        /// The outermost of `spans`: those no other one holds.
        func outermost(_ spans: [Span]) -> [Span] {
            // Outermost first, so a span inside another comes after it.
            var kept: [Span] = []
            for span in spans {
                if let last = kept.last, MarkdownFormatting.contains(last.range, span.range) {
                    continue
                }
                kept.append(span)
            }
            return kept
        }

        /// The unwrap of `spans`: their markers deleted.
        func unwrapping(_ spans: [Span]) -> ParagraphEdit {
            var edit = ParagraphEdit()
            for span in spans {
                edit.deletions += span.markers
                edit.unwrapped.append(span.range)
            }
            return edit
        }

        /// An empty `marker` pair around `caret` — what a toggle at a
        /// caret inserts — as literal text, not syntax.
        func emptyPair(around caret: Int, marker: Marker) -> [NSRange]? {
            let m = marker.text.utf16.count
            let token = marker.text.utf16.first
            let at = caret - range.location
            var left = 0
            while at - left > 0, units[at - left - 1] == token {
                left += 1
            }
            var right = 0
            while at + right < units.count, units[at + right] == token {
                right += 1
            }
            guard left == m, right == m,
                  (at - m ..< at + m).allSatisfy({ !self.marker[$0] }) else {
                return nil
            }
            return [
                NSRange(location: caret - m, length: m),
                NSRange(location: caret, length: m),
            ]
        }

        // MARK: checking

        /// Whether `edit` changes only what it means to. Parsed again,
        /// every syntax character it keeps is still syntax, every
        /// marker it adds is syntax, every empty pair stays literal,
        /// and every text character keeps its other style and has
        /// `style` exactly where the edit puts it. An edit with pairs
        /// is checked again with a letter typed between each pair's
        /// markers: the letter must take the style, nothing else
        /// change.
        func accepts(_ edit: ParagraphEdit, style: MarkdownStyler.Style) -> Bool {
            guard check(edit, style: style, typing: .none) else { return false }
            return edit.pairs.isEmpty || check(edit, style: style, typing: .letter)
        }

        private func check(
            _ edit: ParagraphEdit,
            style: MarkdownStyler.Style,
            typing: Typing
        ) -> Bool {
            let base = range.location
            let count = units.count
            var deleted = [Bool](repeating: false, count: count)
            for deletion in edit.deletions {
                for position in deletion.location ..< NSMaxRange(deletion) {
                    deleted[position - base] = true
                }
            }

            /// What goes in before a position, and whether it's an
            /// empty pair.
            var inserted: [Int: (units: [UInt16], isPair: Bool)] = [:]
            for insertion in edit.insertions {
                inserted[insertion.position - base] = (Array(insertion.text.utf16), false)
            }
            for pair in edit.pairs {
                inserted[pair.position - base] = (Array(pair.text.utf16), true)
            }

            // The new text, where each kept character went, the added
            // markers that must be syntax, those that must stay
            // literal, and the letters typed into pairs.
            var result: [UInt16] = []
            var moved = [Int](repeating: 0, count: count)
            var syntax: [Int] = []
            var literal: [Int] = []
            var typed: [Int] = []
            for index in 0 ... count {
                if let insertion = inserted[index] {
                    var markers = insertion.units
                    let isPair = insertion.isPair
                    var letter: Int?
                    if isPair, typing == .letter {
                        letter = result.count + markers.count / 2
                        markers.insert(Self.typedLetter, at: markers.count / 2)
                    }
                    for position in result.count ..< result.count + markers.count {
                        if position == letter {
                            typed.append(position)
                        } else if isPair, typing == .none {
                            literal.append(position)
                        } else {
                            syntax.append(position)
                        }
                    }
                    result += markers
                }
                guard index < count else { break }
                if !deleted[index] {
                    moved[index] = result.count
                    result.append(units[index])
                }
            }
            let after = Paragraph(text: String(decoding: result, as: UTF16.self), location: 0)
            guard after.units.count == result.count else { return false }

            // The style afterwards: the spans that survive, plus the
            // windows wrapped.
            let unwrapped = Set(edit.unwrapped.map(\.location))
            let survivors = spans.filter {
                $0.style == style && !unwrapped.contains($0.range.location)
            }
            let expected = Self.mask(
                (survivors.map(\.range) + edit.wrapped).map {
                    $0.location - base ..< NSMaxRange($0) - base
                },
                count: count
            )

            let other: MarkdownStyler.Style = style == .strong ? .emphasis : .strong
            let otherBefore = covered(by: other)
            let otherAfter = after.covered(by: other)
            let styleAfter = after.covered(by: style)
            for index in 0 ..< count where !deleted[index] {
                let target = moved[index]
                if marker[index] {
                    guard after.marker[target] else { return false }
                    continue
                }
                if isWhitespace(units[index]) {
                    continue
                }
                guard !after.marker[target],
                      otherAfter[target] == otherBefore[index],
                      styleAfter[target] == expected[index] else {
                    return false
                }
            }
            return syntax.allSatisfy { after.marker[$0] }
                && literal.allSatisfy { !after.marker[$0] }
                && typed.allSatisfy { !after.marker[$0] && styleAfter[$0] }
        }

        /// What a pair check types between the markers.
        private static let typedLetter = UInt16(UInt8(ascii: "x"))
    }

    /// How far a wrap reaches: the selected text, or the words around
    /// it — markers between a letter and punctuation can't open or
    /// close.
    private enum Reach {
        case selection
        case words
    }

    /// What a wrap does with spans of its style inside it.
    private enum InnerSpans {
        /// Their markers go — `**a** b` → `**a b**`.
        case dissolve
        /// They stay nested, when dissolving would break the syntax
        /// beside them.
        case keep
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
        /// The paragraph read last — selections arrive sorted, so the
        /// next one usually falls in it, and finding a paragraph's
        /// bounds scans it.
        private var last: Paragraph?

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
            if let last, NSLocationInRange(location, last.range) {
                return last
            }
            let range = source.paragraphRange(for: NSRange(location: location, length: 0))
            let paragraph = Paragraph(
                text: source.substring(with: range), location: range.location
            )
            last = paragraph
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
    }

    // MARK: planning the edit

    /// The command's accepted changes before they become
    /// replacements.
    private struct Plan {
        let marker: Marker
        /// Marker stretches to delete, possibly overlapping.
        private var deletions: [NSRange] = []
        private var insertions: [(position: Int, text: String)] = []
        /// The stretches rewritten — a selection touching one grows
        /// over it.
        private var regions: [NSRange] = []
        /// Each paragraph's accepted edit so far, by its start: a
        /// later edit there is checked together with it.
        private var accepted: [Int: ParagraphEdit] = [:]
        /// Each adopted edit's extent: a later edit meeting one is
        /// refused, so replacements never overlap.
        private var claimed: [NSRange] = []
        /// Carets given a marker pair: they land between the markers.
        private var pairedCarets = Set<Int>()
        /// Carets whose word gained an opening marker right at them:
        /// they move past it, into the word's new span.
        private var openingCarets = Set<Int>()

        init(marker: Marker) {
            self.marker = marker
        }

        /// Unwrapping: each span of the style holding a selected text
        /// character loses its markers.
        mutating func unwrap(_ pieces: [Piece]) {
            for group in byParagraph(pieces) {
                unwrapParagraph(group)
            }
        }

        /// Wrapping: each piece without the style is wrapped from its
        /// first to its last text character.
        mutating func wrap(_ pieces: [Piece]) {
            let unstyled = pieces.filter { !$0.isStyled(marker.style) }
            for group in byParagraph(unstyled) {
                wrapParagraph(group)
            }
        }

        /// One paragraph's unwrap — of every span holding the text, or
        /// failing that of the outermost ones, the inverse of a wrap
        /// that had to keep spans nested.
        @discardableResult
        private mutating func unwrapParagraph(_ group: [Piece]) -> Bool {
            let paragraph = group[0].paragraph
            var seen = Set<Int>()
            var spans: [Span] = []
            for piece in group {
                for span in paragraph.spans where span.style == marker.style
                    && piece.hasContent(in: span.range)
                    && seen.insert(span.range.location).inserted {
                    spans.append(span)
                }
            }
            if adopt(paragraph.unwrapping(spans), in: paragraph) {
                return true
            }
            let outer = paragraph.outermost(spans)
            guard outer.count < spans.count else { return false }
            return adopt(paragraph.unwrapping(outer), in: paragraph)
        }

        /// One paragraph's wrap, in the first form whose result parses
        /// as intended.
        @discardableResult
        private mutating func wrapParagraph(_ group: [Piece]) -> Bool {
            let attempts: [(Reach, InnerSpans)] = [
                (.selection, .dissolve), (.selection, .keep),
                (.words, .dissolve), (.words, .keep),
            ]
            let paragraph = group[0].paragraph
            let windows = group.map(\.window)
            for (reach, inner) in attempts {
                let edit = paragraph.wrapping(
                    windows, marker: marker, reach: reach, inner: inner
                )
                if adopt(edit, in: paragraph) {
                    return true
                }
            }
            return false
        }

        /// A caret toggle — see the type's notes: an empty pair comes
        /// out, a word holding the caret toggles, else a pair goes in,
        /// else the word beside the caret toggles.
        mutating func toggle(caret: Int, reader: Reader) {
            let paragraph = reader.paragraph(at: caret)
            if let pair = paragraph.emptyPair(around: caret, marker: marker) {
                var removal = ParagraphEdit()
                removal.deletions = pair
                if adopt(removal, in: paragraph) {
                    return
                }
            }
            if paragraph.isInsideWord(caret) {
                let word = paragraph.widenedToWords(NSRange(location: caret, length: 0))
                if toggle(word: word, caret: caret, reader: reader) {
                    return
                }
            }

            var pair = ParagraphEdit()
            pair.pairs = [(caret, marker.text + marker.text)]
            if adopt(pair, in: paragraph) {
                pairedCarets.insert(caret)
                return
            }
            if let word = paragraph.word(beside: caret) {
                toggle(word: word, caret: caret, reader: reader)
            }
        }

        /// `word` toggled as if selected; `caret` moves into the word's
        /// new span if its opening marker lands right at it.
        @discardableResult
        private mutating func toggle(word: NSRange, caret: Int, reader: Reader) -> Bool {
            let pieces = reader.pieces(of: word).filter { !$0.content.isEmpty }
            guard !pieces.isEmpty else { return false }
            // A word lies in one paragraph: one piece.
            let toggled = pieces[0].isStyled(marker.style)
                ? unwrapParagraph(pieces)
                : wrapParagraph(pieces)
            let key = pieces[0].paragraph.range.location
            if toggled, accepted[key]?.wrapped.contains(where: { $0.location == caret }) == true {
                openingCarets.insert(caret)
            }
            return toggled
        }

        /// Takes `edit` if it meets no edit taken before and, together
        /// with the paragraph's edits taken so far, parses as intended.
        private mutating func adopt(_ edit: ParagraphEdit, in paragraph: Paragraph) -> Bool {
            let points = edit.regions + edit.deletions
                + (edit.insertions + edit.pairs).map { NSRange(location: $0.position, length: 0) }
            guard let first = points.first else { return false }
            let extent = points.dropFirst().reduce(first) { NSUnionRange($0, $1) }
            let meets = claimed.contains {
                $0.location <= NSMaxRange(extent) && extent.location <= NSMaxRange($0)
            }
            let key = paragraph.range.location
            let combined = (accepted[key] ?? ParagraphEdit()).adding(edit)
            guard !meets, paragraph.accepts(combined, style: marker.style) else {
                return false
            }
            accepted[key] = combined
            deletions += edit.deletions
            insertions += edit.insertions + edit.pairs
            regions += edit.regions
            claimed.append(extent)
            return true
        }

        /// `pieces` grouped by paragraph — selections arrive sorted,
        /// so a paragraph's pieces are adjacent.
        private func byParagraph(_ pieces: [Piece]) -> [[Piece]] {
            var groups: [[Piece]] = []
            for piece in pieces {
                if let last = groups.last?.last, last.paragraph.range == piece.paragraph.range {
                    groups[groups.count - 1].append(piece)
                } else {
                    groups.append([piece])
                }
            }
            return groups
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

        /// Where each of `selections` lands: a range grows over the
        /// stretches rewritten around it; a caret given a pair sits
        /// between the markers. Binary searches, so many selections
        /// over many regions stay linear-logarithmic.
        func selections(
            after selections: [NSRange],
            through replacements: [Replacement]
        ) -> [NSRange] {
            let translation = Translation(replacements)
            let rewritten = merged(regions)
            let after = selections.map { selection -> NSRange in
                guard selection.length > 0 else {
                    let edge: Edge = openingCarets.contains(selection.location) ? .end : .start
                    let caret = translation.position(of: selection.location, edge: edge)
                    let offset = pairedCarets.contains(selection.location)
                        ? marker.text.utf16.count
                        : 0
                    return NSRange(location: caret + offset, length: 0)
                }

                var affected = selection
                var index = firstIndex(in: rewritten) { NSMaxRange($0) > selection.location }
                while index < rewritten.count, rewritten[index].location < NSMaxRange(selection) {
                    affected = NSUnionRange(affected, rewritten[index])
                    index += 1
                }
                let start = translation.position(of: affected.location, edge: .start)
                let end = translation.position(of: NSMaxRange(affected), edge: .end)
                return NSRange(location: start, length: end - start)
            }
            return merged(after)
        }
    }

    // MARK: selections after

    /// Which end of a selection a position is: a start stays before
    /// text inserted at it, an end moves past it.
    private enum Edge {
        case start
        case end
    }

    /// Pre-edit positions to post-edit ones, by binary search over the
    /// replacements' running length change.
    private struct Translation {
        private let replacements: [Replacement]
        /// `shifts[k]`: the length change of replacements `0 ..< k`.
        private let shifts: [Int]

        init(_ replacements: [Replacement]) {
            self.replacements = replacements
            var shifts = [0]
            for replacement in replacements {
                shifts.append(
                    shifts[shifts.count - 1] + replacement.string.utf16.count
                        - replacement.range.length
                )
            }
            self.shifts = shifts
        }

        /// Where `position` lands; inside a replacement it snaps to
        /// the replacement's start or end.
        func position(of position: Int, edge: Edge) -> Int {
            let index = firstIndex(in: replacements) {
                let end = NSMaxRange($0.range)
                let before = end < position
                    || (end == position && (edge == .end || $0.range.location < position))
                return !before
            }
            let shift = shifts[index]
            guard index < replacements.count,
                  replacements[index].range.location < position else {
                return position + shift
            }
            let replacement = replacements[index]
            let length = edge == .end ? replacement.string.utf16.count : 0
            return replacement.range.location + shift + length
        }
    }

    /// The first index of sorted `items` where `isPast` holds — it
    /// must be false then true along them.
    private static func firstIndex<T>(in items: [T], where isPast: (T) -> Bool) -> Int {
        var low = 0
        var high = items.count
        while low < high {
            let mid = (low + high) / 2
            if isPast(items[mid]) {
                high = mid
            } else {
                low = mid + 1
            }
        }
        return low
    }

    private static func contains(_ outer: NSRange, _ inner: NSRange) -> Bool {
        outer.location <= inner.location && NSMaxRange(inner) <= NSMaxRange(outer)
    }

    /// `ranges` sorted, overlapping or equal ones joined — two carets
    /// landing together become one; touching ones stay apart.
    private static func merged(_ ranges: [NSRange]) -> [NSRange] {
        var result: [NSRange] = []
        for range in ranges.sorted(by: { $0.location < $1.location }) {
            guard let last = result.last,
                  NSIntersectionRange(last, range).length > 0 || last == range else {
                result.append(range)
                continue
            }
            result[result.count - 1] = NSUnionRange(last, range)
        }
        return result
    }

    /// `ranges` sorted, overlapping or touching ones joined.
    private static func joined(_ ranges: [NSRange]) -> [NSRange] {
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

    /// CommonMark whitespace, as the styler reads it; a surrogate
    /// half is part of a character that isn't.
    private static func isWhitespace(_ unit: unichar) -> Bool {
        guard let scalar = Unicode.Scalar(unit) else { return false }
        return MarkdownStyler.isWhitespace(scalar)
    }
}
