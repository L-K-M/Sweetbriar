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
// after it, and emphasis never crosses a paragraph. Emphasis follows
// CommonMark's delimiter-run algorithm — flanking, the `_` rules, the
// rule of three — so `***x***`, `*a **b***` and `**a*b***` pair as
// CommonMark pairs them.

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
        for pair in emphasisPairs(u, protected: protected) {
            appendPair(pair, runs: &runs)
        }
        return runs
    }

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

    /// A run of `*` or `_` that may open or close emphasis, consumed
    /// as it pairs: openers from their inner (right) end, closers
    /// from their inner (left) end.
    private struct Delimiter {
        let token: UInt16
        /// The run's length before any pairing — the rule of three
        /// reads it.
        let originalCount: Int
        let canOpen: Bool
        let canClose: Bool
        var start: Int
        var count: Int
    }

    /// The strong and emphasis spans, outermost first — CommonMark's
    /// delimiter-run algorithm in one pass. Each closer pairs with the
    /// nearest compatible opener of its character, two characters at
    /// a time while both runs have two (strong), else one (emphasis);
    /// openers between the two can never pair and drop out. One stack
    /// of openers per character, with CommonMark's "openers bottom"
    /// marks, keeps the work linear: unmatched markers never rescan
    /// the paragraph.
    private static func emphasisPairs(_ u: [UInt16], protected: [Bool]) -> [Pair] {
        /// The stacks' slot for each character: `*` then `_`.
        func slot(_ token: UInt16) -> Int {
            token == Char.asterisk ? 0 : 1
        }
        /// Closer classes: original length mod 3, times can-open.
        let classes = 6

        var pairs: [Pair] = []
        var stacks: [[Delimiter]] = [[], []]
        /// Per character and closer class: the stack depth below
        /// which no opener pairs with that class.
        var bottoms = [[Int]](repeating: Array(repeating: 0, count: classes), count: 2)

        /// Shrinks a stack to `depth`, lowering its bottoms.
        func truncate(_ s: Int, to depth: Int) {
            guard depth < stacks[s].count else { return }
            stacks[s].removeLast(stacks[s].count - depth)
            bottoms[s] = bottoms[s].map { min($0, depth) }
        }

        var i = 0
        while i < u.count {
            guard !protected[i],
                  u[i] == Char.asterisk || u[i] == Char.underscore else {
                i += 1
                continue
            }
            var end = i + 1
            while end < u.count, u[end] == u[i], !protected[end] {
                end += 1
            }
            var closer = delimiter(u, run: i ..< end)
            i = end

            let s = slot(closer.token)
            let other = 1 - s
            let closerClass = (closer.originalCount % 3) * 2 + (closer.canOpen ? 1 : 0)
            while closer.canClose, closer.count > 0 {
                let candidates = stride(
                    from: stacks[s].count - 1, through: bottoms[s][closerClass], by: -1
                )
                guard let index = candidates.first(where: {
                    canPair(stacks[s][$0], with: closer)
                }) else {
                    // Nothing below pairs with this class: later
                    // closers like it start their search here.
                    bottoms[s][closerClass] = stacks[s].count
                    break
                }

                // Openers between the pair drop out — of this
                // character above it, of the other after it.
                truncate(s, to: index + 1)
                var opener = stacks[s][index]
                var otherDepth = stacks[other].count
                while otherDepth > 0, stacks[other][otherDepth - 1].start > opener.start {
                    otherDepth -= 1
                }
                truncate(other, to: otherDepth)

                let used = min(opener.count, closer.count) >= 2 ? 2 : 1
                opener.count -= used
                pairs.append(Pair(
                    style: used == 2 ? .strong : .emphasis,
                    range: opener.start + opener.count ..< closer.start + used
                ))
                closer.start += used
                closer.count -= used

                if opener.count == 0 {
                    truncate(s, to: index)
                } else {
                    stacks[s][index] = opener
                }
            }
            if closer.canOpen, closer.count > 0 {
                stacks[s].append(closer)
            }
        }

        // Outermost first, so a nested span follows the one holding it.
        return pairs.sorted {
            ($0.range.lowerBound, -$0.range.count) < ($1.range.lowerBound, -$1.range.count)
        }
    }

    /// Whether `opener` may pair with `closer` — CommonMark's rule of
    /// three: when either run can both open and close, their original
    /// lengths may not sum to a multiple of three unless both are
    /// multiples of three (so `*foo**bar**baz*` nests).
    private static func canPair(_ opener: Delimiter, with closer: Delimiter) -> Bool {
        guard opener.canClose || closer.canOpen else { return true }
        let sum = opener.originalCount + closer.originalCount
        return sum % 3 != 0
            || (opener.originalCount % 3 == 0 && closer.originalCount % 3 == 0)
    }

    /// The delimiter `run` makes — CommonMark's flanking rules. A run
    /// is left-flanking (may open) when text, not space, follows it,
    /// and right-flanking (may close) when text precedes it;
    /// punctuation counts as text only beside space or punctuation.
    /// `_` inside a word neither opens nor closes (snake_case). The
    /// neighbours are whole characters: an emoji beside a run is a
    /// symbol, not two unknown code units.
    private static func delimiter(_ u: [UInt16], run: Range<Int>) -> Delimiter {
        let before = kind(ofScalarEndingAt: run.lowerBound, in: u)
        let after = kind(ofScalarStartingAt: run.upperBound, in: u)
        let leftFlanking = after != .whitespace
            && (after != .punctuation || before != .text)
        let rightFlanking = before != .whitespace
            && (before != .punctuation || after != .text)

        let token = u[run.lowerBound]
        let underscore = token == Char.underscore
        return Delimiter(
            token: token,
            originalCount: run.count,
            canOpen: leftFlanking
                && (!underscore || !rightFlanking || before == .punctuation),
            canClose: rightFlanking
                && (!underscore || !leftFlanking || after == .punctuation),
            start: run.lowerBound,
            count: run.count
        )
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

    private static func isDigit(_ u: UInt16) -> Bool {
        (0x30 ... 0x39).contains(u)
    }

    /// What flanking needs to know of a delimiter run's neighbour.
    private enum Kind {
        /// CommonMark's Unicode whitespace — or the paragraph's edge.
        case whitespace
        /// CommonMark's Unicode punctuation: categories P and S.
        case punctuation
        case text
    }

    /// CommonMark's Unicode punctuation: general categories P and S.
    private static let punctuation = CharacterSet.punctuationCharacters
        .union(.symbols)

    /// The kind of the character just before `end` — a surrogate pair
    /// read as one scalar.
    private static func kind(ofScalarEndingAt end: Int, in u: [UInt16]) -> Kind {
        guard end > 0 else { return .whitespace }
        let low = u[end - 1]
        if UTF16.isTrailSurrogate(low), end > 1, UTF16.isLeadSurrogate(u[end - 2]) {
            return kind(of: scalar(lead: u[end - 2], trail: low))
        }
        return kind(of: Unicode.Scalar(low))
    }

    /// The kind of the character starting at `start`.
    private static func kind(ofScalarStartingAt start: Int, in u: [UInt16]) -> Kind {
        guard start < u.count else { return .whitespace }
        let high = u[start]
        if UTF16.isLeadSurrogate(high), start + 1 < u.count, UTF16.isTrailSurrogate(u[start + 1]) {
            return kind(of: scalar(lead: high, trail: u[start + 1]))
        }
        return kind(of: Unicode.Scalar(high))
    }

    /// The scalar a surrogate pair encodes.
    private static func scalar(lead: UInt16, trail: UInt16) -> Unicode.Scalar? {
        let high = UInt32(lead) - 0xD800
        let low = UInt32(trail) - 0xDC00
        return Unicode.Scalar(0x10000 + (high << 10) + low)
    }

    /// A lone surrogate counts as text.
    private static func kind(of scalar: Unicode.Scalar?) -> Kind {
        guard let scalar else { return .text }
        if CharacterSet.whitespacesAndNewlines.contains(scalar) {
            return .whitespace
        }
        return punctuation.contains(scalar) ? .punctuation : .text
    }
}
