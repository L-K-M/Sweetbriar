// The composer's Markdown style-run generator (plan M0.11, spec/08
// "Composer"). TextKit hands the display delegate one paragraph at a
// time; for each it returns the ranges that carry a heading, strong,
// emphasis or list-item style plus the syntax characters themselves
// as `.marker` runs, so the view can dim them while the source stays
// visible. The styling is presentation only and never stored. Pure
// and Foundation-only — hand-rolled, no regex compiled per call — so
// it type-checks and tests on Linux.
//
// It reads a subset of CommonMark, enough for a composer: an ATX
// heading needs a space after its `#`s, a list marker needs text
// after it, and emphasis never crosses a paragraph. Delimiter runs
// pair `*`/`**` and `***` (`***x***`, `***x** y*`, `***x* y**`), not
// CommonMark's full delimiter algorithm.

import Foundation

/// Live styling for one paragraph of Markdown source.
enum MarkdownStyler {
    /// What one range of the paragraph means to the display layer.
    enum Style: Equatable {
        /// An ATX heading `#`…`######` followed by a space; the
        /// payload is the level (1–6) and the run covers the line.
        case heading(level: Int)
        /// `**x**` or `__x__`, markers included — two characters
        /// each side.
        case strong
        /// `*x*` or `_x_`, markers included — one character each side.
        case emphasis
        /// A `- `, `* `, `+ `, `1. ` or `1) ` line, marker included —
        /// the run covers the line so the view can hang its indent.
        case listItem
        /// The syntax characters themselves (`#`, `**`, `*`, `__`,
        /// `_`, `-`, `1.`): dimmed, never hidden.
        case marker
    }

    /// One styled range of the paragraph.
    struct Run: Equatable {
        /// The UTF-16 range the style covers, in paragraph
        /// coordinates — TextKit 2's delegate hands paragraphs over
        /// as `NSRange`s, so runs stay in the same units.
        let range: NSRange
        let style: Style
    }

    /// The runs for `paragraph`. Block styles come first (one run per
    /// line's heading or list item plus its `.marker`), then the
    /// inline styles in scan order — a nested run always follows the
    /// run containing it, so the view can layer fonts.
    static func styles(forParagraph paragraph: String) -> [Run] {
        let u = Array(paragraph.utf16)
        guard !u.isEmpty else { return [] }

        var runs: [Run] = []
        /// Positions a line-level construct already claimed — the
        /// inline pass must not read a `* ` bullet as emphasis.
        var protected = [Bool](repeating: false, count: u.count)
        blockRuns(u, runs: &runs, protected: &protected)
        let closers = Closers(u, protected: protected)
        inlineRuns(
            u, in: 0 ..< u.count,
            protected: protected, closers: closers, runs: &runs
        )
        return runs
    }

    /// The longest delimiter run the inline pass pairs: `***`.
    private static let maxDelimiterRun = 3

    /// Marker and boundary characters as UTF-16 code units.
    private enum Char {
        static let hash: UInt16 = 0x23 // '#'
        static let asterisk: UInt16 = 0x2A // '*'
        static let underscore: UInt16 = 0x5F // '_'
        static let hyphen: UInt16 = 0x2D // '-'
        static let plus: UInt16 = 0x2B // '+'
        static let period: UInt16 = 0x2E // '.'
        static let paren: UInt16 = 0x29 // ')'
        static let space: UInt16 = 0x20
        static let newline: UInt16 = 0x0A
    }

    // MARK: line-level constructs

    /// `#`-headings and list markers, valid only at a line start
    /// after up to three spaces of indent (CommonMark's rule — four
    /// would open a code block).
    private static func blockRuns(
        _ u: [UInt16],
        runs: inout [Run],
        protected: inout [Bool]
    ) {
        var lineStart = 0
        while lineStart < u.count {
            var lineEnd = lineStart
            while lineEnd < u.count, u[lineEnd] != Char.newline {
                lineEnd += 1
            }
            lineRuns(
                u, at: lineStart ..< lineEnd,
                runs: &runs, protected: &protected
            )
            lineStart = lineEnd + 1
        }
    }

    /// One line's block style: a heading or a list item, whichever
    /// the character after the indent opens.
    private static func lineRuns(
        _ u: [UInt16],
        at line: Range<Int>,
        runs: inout [Run],
        protected: inout [Bool]
    ) {
        let maxIndent = 3
        var i = line.lowerBound
        var indent = 0
        while i < line.upperBound, u[i] == Char.space, indent <= maxIndent {
            i += 1
            indent += 1
        }
        guard indent <= maxIndent, i < line.upperBound else { return }

        if u[i] == Char.hash {
            headingRuns(u, at: line, hashStart: i, runs: &runs, protected: &protected)
        } else {
            listRuns(u, at: line, markerStart: i, runs: &runs, protected: &protected)
        }
    }

    /// `#`…`######` followed by a space styles the whole line at that
    /// level; the `#`s are the `.marker` run.
    private static func headingRuns(
        _ u: [UInt16],
        at line: Range<Int>,
        hashStart: Int,
        runs: inout [Run],
        protected: inout [Bool]
    ) {
        let maxLevel = 6
        var level = 0
        var i = hashStart
        while i < line.upperBound, u[i] == Char.hash {
            i += 1
            level += 1
        }
        guard level > 0, level <= maxLevel,
              i < line.upperBound, u[i] == Char.space else {
            return
        }
        runs.append(Run(
            range: NSRange(location: line.lowerBound, length: line.count),
            style: .heading(level: level)
        ))
        runs.append(Run(
            range: NSRange(location: hashStart, length: level),
            style: .marker
        ))
        for p in hashStart ..< i {
            protected[p] = true
        }
    }

    /// `- `, `* `, `+ ` and `1. `/`1) ` open a list item; the whole
    /// line takes `.listItem` and the marker token is its own
    /// `.marker` run.
    private static func listRuns(
        _ u: [UInt16],
        at line: Range<Int>,
        markerStart: Int,
        runs: inout [Run],
        protected: inout [Bool]
    ) {
        var markerEnd = markerStart + 1
        switch u[markerStart] {
        case Char.hyphen, Char.plus, Char.asterisk:
            break
        default:
            // Ordered markers: one to nine digits then `.` or `)`
            // (CommonMark caps the number's width at nine digits).
            let maxDigits = 9
            var digitEnd = markerStart
            while digitEnd < line.upperBound, isDigit(u[digitEnd]) {
                digitEnd += 1
            }
            let digits = digitEnd - markerStart
            guard digits >= 1, digits <= maxDigits,
                  digitEnd < line.upperBound,
                  u[digitEnd] == Char.period || u[digitEnd] == Char.paren else {
                return
            }
            markerEnd = digitEnd + 1
        }
        guard markerEnd < line.upperBound, u[markerEnd] == Char.space else {
            return
        }
        runs.append(Run(
            range: NSRange(location: line.lowerBound, length: line.count),
            style: .listItem
        ))
        runs.append(Run(
            range: NSRange(location: markerStart, length: markerEnd - markerStart),
            style: .marker
        ))
        for p in markerStart ..< markerEnd {
            protected[p] = true
        }
    }

    // MARK: inline constructs

    /// Every usable close token in the paragraph by marker character
    /// and run length, ascending — found in one pass, so pairing an
    /// opener is a binary search rather than a rescan of the rest of
    /// the paragraph (`*a *a *a …`, every opener unmatched, was
    /// quadratic).
    private struct Closers {
        private struct Key: Hashable {
            let token: UInt16
            let length: Int
        }

        private var positions: [Key: [Int]] = [:]

        init(_ u: [UInt16], protected: [Bool]) {
            for j in u.indices
            where u[j] == Char.asterisk || u[j] == Char.underscore {
                for length in 1 ... maxDelimiterRun
                where canClose(u, at: j, length: length, protected: protected) {
                    positions[Key(token: u[j], length: length), default: []]
                        .append(j)
                }
            }
        }

        /// The first close of `length` × `token` at or after `start`
        /// that ends inside `span`, or nil.
        func first(
            _ token: UInt16,
            length: Int,
            from start: Int,
            in span: Range<Int>
        ) -> Int? {
            guard let list = positions[Key(token: token, length: length)] else {
                return nil
            }
            var low = 0
            var high = list.count
            while low < high {
                let mid = (low + high) / 2
                if list[mid] < start {
                    low = mid + 1
                } else {
                    high = mid
                }
            }
            guard low < list.count, list[low] + length <= span.upperBound else {
                return nil
            }
            return list[low]
        }
    }

    /// Strong and emphasis inside `span`, skipping `protected`
    /// positions and recursing into each matched pair so `**_x_**`
    /// takes both styles. A marker with no later partner styles
    /// nothing — unclosed syntax stays literal.
    private static func inlineRuns(
        _ u: [UInt16],
        in span: Range<Int>,
        protected: [Bool],
        closers: Closers,
        runs: inout [Run]
    ) {
        var i = span.lowerBound
        while i < span.upperBound {
            guard !protected[i],
                  u[i] == Char.asterisk || u[i] == Char.underscore else {
                i += 1
                continue
            }
            // The delimiter run, up to `***`. A run that fails to
            // pair is literal as a whole, never split into singletons.
            let token = u[i]
            var length = 1
            while length < maxDelimiterRun,
                  i + length < span.upperBound,
                  u[i + length] == token {
                length += 1
            }
            guard canOpen(u, at: i, length: length) else {
                i += length
                continue
            }

            let pairing = Pairing(
                u, opener: i, length: length,
                token: token, span: span, closers: closers
            )
            guard let pairing else {
                i += length
                continue
            }
            for pair in pairing.pairs {
                appendPair(pair, runs: &runs)
            }
            for inner in pairing.interiors {
                inlineRuns(
                    u, in: inner,
                    protected: protected, closers: closers, runs: &runs
                )
            }
            i = pairing.end
        }
    }

    /// One matched span: its style and its extent, markers included.
    private struct Pair {
        let style: Style
        let range: Range<Int>

        /// The marker length each side: two for strong, one for
        /// emphasis.
        var markerLength: Int {
            style == .strong ? 2 : 1
        }
    }

    /// How a delimiter run pairs with the closers after it — the
    /// spans it opens, outermost first, the text between their
    /// markers left to scan, and where scanning resumes.
    private struct Pairing {
        let pairs: [Pair]
        let interiors: [Range<Int>]
        let end: Int

        /// Nil when nothing closes the run.
        /// - `*`/`**`: the next closer of the same length.
        /// - `***`: whichever closes first. `***` makes emphasis
        ///   around strong; `**` closes strong inside and leaves the
        ///   outer `*` to the next `*`; `*` closes emphasis inside and
        ///   leaves the outer `**` to the next `**`. An outer marker
        ///   with no partner stays literal.
        init?(
            _ u: [UInt16],
            opener i: Int,
            length: Int,
            token: UInt16,
            span: Range<Int>,
            closers: Closers
        ) {
            func close(_ length: Int, from start: Int) -> Int? {
                closers.first(token, length: length, from: start, in: span)
            }

            switch length {
            case 1, 2:
                guard let c = close(length, from: i + length) else {
                    return nil
                }
                pairs = [Pair(
                    style: length == 2 ? .strong : .emphasis,
                    range: i ..< c + length
                )]
                interiors = [i + length ..< c]
                end = c + length
            default:
                let c3 = close(3, from: i + 3)
                let c2 = close(2, from: i + 3)
                let c1 = close(1, from: i + 3)
                // The earliest closer wins; a `***` closer beats the
                // `**` starting at the same place.
                let earliest = [c3, c2, c1].compactMap { $0 }.min()
                guard let earliest else { return nil }

                if earliest == c3 {
                    pairs = [
                        Pair(style: .emphasis, range: i ..< earliest + 3),
                        Pair(style: .strong, range: i + 1 ..< earliest + 2),
                    ]
                    interiors = [i + 3 ..< earliest]
                    end = earliest + 3
                } else if earliest == c2 {
                    let inner = Pair(style: .strong, range: i + 1 ..< earliest + 2)
                    guard let outer = close(1, from: earliest + 2) else {
                        pairs = [inner]
                        interiors = [i + 3 ..< earliest]
                        end = earliest + 2
                        return
                    }
                    pairs = [Pair(style: .emphasis, range: i ..< outer + 1), inner]
                    interiors = [i + 3 ..< earliest, earliest + 2 ..< outer]
                    end = outer + 1
                } else {
                    let inner = Pair(style: .emphasis, range: i + 2 ..< earliest + 1)
                    guard let outer = close(2, from: earliest + 1) else {
                        pairs = [inner]
                        interiors = [i + 3 ..< earliest]
                        end = earliest + 1
                        return
                    }
                    pairs = [Pair(style: .strong, range: i ..< outer + 2), inner]
                    interiors = [i + 3 ..< earliest, earliest + 1 ..< outer]
                    end = outer + 2
                }
            }
        }
    }

    /// A pair's style run followed by its two `.marker` runs.
    private static func appendPair(_ pair: Pair, runs: inout [Run]) {
        let m = pair.markerLength
        runs.append(Run(
            range: NSRange(location: pair.range.lowerBound, length: pair.range.count),
            style: pair.style
        ))
        runs.append(Run(
            range: NSRange(location: pair.range.lowerBound, length: m),
            style: .marker
        ))
        runs.append(Run(
            range: NSRange(location: pair.range.upperBound - m, length: m),
            style: .marker
        ))
    }

    /// Whether the marker at `i` may open: a space right after it
    /// can't open (CommonMark's flanking rule), and `_`/`__` between
    /// two alphanumerics is a word character, never emphasis.
    private static func canOpen(_ u: [UInt16], at i: Int, length: Int) -> Bool {
        guard i + length < u.count, !isWhitespace(u[i + length]) else {
            return false
        }
        return !isIntraword(u, at: i, length: length)
    }

    /// Whether `length` × the marker at `j` may close: unclaimed by a
    /// line-level construct, not a lone marker inside a longer run
    /// (so `*a **b** c*` keeps its outer emphasis), not after a space,
    /// and not intraword.
    private static func canClose(
        _ u: [UInt16],
        at j: Int,
        length: Int,
        protected: [Bool]
    ) -> Bool {
        let token = u[j]
        guard j + length <= u.count,
              (j ..< j + length).allSatisfy({ u[$0] == token && !protected[$0] }) else {
            return false
        }
        let insideRun = length == 1 &&
            ((j > 0 && u[j - 1] == token) || (j + 1 < u.count && u[j + 1] == token))
        let afterSpace = j > 0 && isWhitespace(u[j - 1])
        return !insideRun && !afterSpace && !isIntraword(u, at: j, length: length)
    }

    /// `_` or `__` at `i` with alphanumerics on both sides is a word
    /// character (snake_case), never emphasis — CommonMark's
    /// intraword rule; `*`/`**` may sit inside a word.
    private static func isIntraword(_ u: [UInt16], at i: Int, length: Int) -> Bool {
        guard u[i] == Char.underscore, i > 0, i + length < u.count else {
            return false
        }
        return isAlphanumeric(u[i - 1]) && isAlphanumeric(u[i + length])
    }

    private static func isDigit(_ u: UInt16) -> Bool {
        (0x30 ... 0x39).contains(u)
    }

    private static func isWhitespace(_ u: UInt16) -> Bool {
        u == Char.space || u == 0x09 || u == Char.newline || u == 0x0D
    }

    private static func isAlphanumeric(_ u: UInt16) -> Bool {
        guard let scalar = Unicode.Scalar(u) else { return false }
        return CharacterSet.alphanumerics.contains(scalar)
    }
}
