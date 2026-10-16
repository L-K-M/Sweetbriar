// The composer's ⌘B/⌘I edit under test (plan M0.11): wrap, unwrap
// with the markers inside or just outside the selection, the empty
// selection's marker pair, the round trip, traits composing, separate
// spans, paragraphs and multiple selections. Foundation only, so it
// runs on Linux too.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
import Foundation
import Testing

@Suite("the Markdown formatting edit")
struct MarkdownFormattingTests {
    /// The edit `marker` computes for `selections` in `text`.
    private func edit(
        _ marker: MarkdownFormatting.Marker = .strong,
        in text: String,
        selections: [NSRange]
    ) -> MarkdownFormatting.Edit {
        MarkdownFormatting.edit(for: marker, in: text, selections: selections)
    }

    /// The edit for one selection.
    private func edit(
        _ marker: MarkdownFormatting.Marker = .strong,
        in text: String,
        selection: NSRange
    ) -> MarkdownFormatting.Edit {
        edit(marker, in: text, selections: [selection])
    }

    /// The string after applying `edit` — what the text storage
    /// would hold. One pass over the ascending, disjoint replacements,
    /// so applying thousands stays linear.
    private func applying(_ edit: MarkdownFormatting.Edit, to text: String) -> String {
        let source = text as NSString
        var result = ""
        var location = 0
        for replacement in edit.replacements {
            result += source.substring(with: NSRange(
                location: location, length: replacement.range.location - location
            ))
            result += replacement.string
            location = NSMaxRange(replacement.range)
        }
        return result + source.substring(from: location)
    }

    /// The text `edit` leaves selected.
    private func selected(_ edit: MarkdownFormatting.Edit, in text: String) -> [String] {
        let result = applying(edit, to: text) as NSString
        return edit.selections.map { result.substring(with: $0) }
    }

    @Test("wrapping a selection covers the wrapped span")
    func wrap() {
        let e = edit(in: "a word c", selection: NSRange(location: 2, length: 4))
        #expect(e.replacements == [
            MarkdownFormatting.Replacement(range: NSRange(location: 2, length: 0), string: "**"),
            MarkdownFormatting.Replacement(range: NSRange(location: 6, length: 0), string: "**"),
        ])
        #expect(applying(e, to: "a word c") == "a **word** c")
        // The new selection covers `**word**`, so ⌘B again unwraps.
        #expect(selected(e, in: "a word c") == ["**word**"])
    }

    @Test("emphasis wraps in a single star")
    func emphasisWrap() {
        let e = edit(.emphasis, in: "word", selection: NSRange(location: 0, length: 4))
        #expect(applying(e, to: "word") == "*word*")
        #expect(e.selections == [NSRange(location: 0, length: 6)])
    }

    @Test("unwrapping markers inside the selection")
    func unwrapInside() {
        let e = edit(in: "a **word** c", selection: NSRange(location: 2, length: 8))
        #expect(applying(e, to: "a **word** c") == "a word c")
        #expect(e.selections == [NSRange(location: 2, length: 4)])

        let u = edit(in: "__a__", selection: NSRange(location: 0, length: 5))
        #expect(applying(u, to: "__a__") == "a")
    }

    @Test("unwrapping markers just outside the selection")
    func unwrapOutside() {
        let e = edit(in: "a **word** c", selection: NSRange(location: 4, length: 4))
        #expect(applying(e, to: "a **word** c") == "a word c")
        // The selection follows the word the dropped markers left.
        #expect(e.selections == [NSRange(location: 2, length: 4)])

        let i = edit(.emphasis, in: "a *word* c", selection: NSRange(location: 3, length: 4))
        #expect(applying(i, to: "a *word* c") == "a word c")
    }

    @Test("a word inside a span unwraps the whole span")
    func insideLongerSpan() {
        // Markdown can't unbold part of a span without splitting it.
        let text = "**hello world**"
        let e = edit(in: text, selection: NSRange(location: 8, length: 5))
        #expect(applying(e, to: text) == "hello world")
        #expect(selected(e, in: text) == ["hello world"])
    }

    @Test("a caret inside a word toggles the word")
    func caretInWord() {
        let e = edit(in: "ab", selection: NSRange(location: 1, length: 0))
        #expect(applying(e, to: "ab") == "**ab**")
        #expect(e.selections == [NSRange(location: 3, length: 0)])

        let i = edit(.emphasis, in: "ab", selection: NSRange(location: 1, length: 0))
        #expect(applying(i, to: "ab") == "*ab*")
        #expect(i.selections == [NSRange(location: 2, length: 0)])
    }

    @Test("a caret beside a word gets a marker pair to type into")
    func caretPair() {
        let e = edit(in: "word next", selection: NSRange(location: 4, length: 0))
        #expect(applying(e, to: "word next") == "word**** next")
        #expect(e.selections == [NSRange(location: 6, length: 0)])
    }

    @Test("italic at a caret in a bold word composes and toggles back")
    func caretInOtherStyle() {
        // A bare pair would re-pair with the bold markers:
        // `**w**ord**`.
        let bold = "**word**"
        let italic = edit(.emphasis, in: bold, selection: NSRange(location: 4, length: 0))
        let both = applying(italic, to: bold)
        #expect(both == "***word***")
        #expect(italic.selections == [NSRange(location: 5, length: 0)])

        let back = edit(.emphasis, in: both, selections: italic.selections)
        #expect(applying(back, to: both) == bold)
        #expect(back.selections == [NSRange(location: 4, length: 0)])
    }

    @Test("a caret edit never breaks the syntax around it")
    func caretKeepsSyntax() {
        // Taking out the literal `**` would glue `__` to `a`, and no
        // other edit here keeps `x` bold: nothing changes.
        let text = "__x__**a"
        let e = edit(.emphasis, in: text, selection: NSRange(location: 6, length: 0))
        #expect(applying(e, to: text) == text)
    }

    @Test("a pair that would re-pair now gives way to the word beside it")
    func pairChecksTheRealText() {
        // `**hello** world**` would take `world`'s bold.
        let bold = "**hello world**"
        let italic = edit(.emphasis, in: bold, selection: NSRange(location: 7, length: 0))
        let both = applying(italic, to: bold)
        #expect(both == "***hello* world**")
        #expect(italic.selections == [NSRange(location: 8, length: 0)])
        let back = edit(.emphasis, in: both, selections: italic.selections)
        #expect(applying(back, to: both) == bold)
        #expect(back.selections == [NSRange(location: 7, length: 0)])

        let emphasized = "*hello world*"
        let bolded = applying(
            edit(in: emphasized, selection: NSRange(location: 6, length: 0)), to: emphasized
        )
        #expect(bolded == "***hello** world*")

        let mixed = "**a** b**c**"
        let mixedItalic = applying(
            edit(.emphasis, in: mixed, selection: NSRange(location: 6, length: 0)), to: mixed
        )
        #expect(mixedItalic == "**a** *b**c***")
    }

    @Test("a caret where a word's markers go lands inside them")
    func caretAtWordStart() {
        let strong = "__word__"
        let italic = edit(.emphasis, in: strong, selection: NSRange(location: 2, length: 0))
        #expect(applying(italic, to: strong) == "__*word*__")
        #expect(italic.selections == [NSRange(location: 3, length: 0)])

        let emphasized = "_word_"
        let bold = edit(in: emphasized, selection: NSRange(location: 1, length: 0))
        #expect(applying(bold, to: emphasized) == "_**word**_")
        #expect(bold.selections == [NSRange(location: 3, length: 0)])
    }

    @Test("carets are checked together, not one by one")
    func caretsTogether() {
        // Two pairs bracketing `word` would make it bold.
        let text = "word"
        let e = edit(.emphasis, in: text, selections: [
            NSRange(location: 0, length: 0), NSRange(location: 4, length: 0),
        ])
        // The first caret gets its pair; the second, which would
        // complete `**word**`, is refused.
        let result = applying(e, to: text)
        #expect(result == "**word")
        #expect(e.selections == [
            NSRange(location: 1, length: 0), NSRange(location: 6, length: 0),
        ])
        #expect(!MarkdownStyler.styles(forParagraph: result).contains { $0.style == .strong })
    }

    @Test("carets that meet never overlap their edits or overrun the text")
    func meetingCarets() {
        // The first caret's edit wins; a later one meeting it does
        // nothing, and carets landing together become one.
        let cases: [(String, [Int], String, [NSRange])] = [
            // A pair at 0 would turn `xa` bold with the literal `****`:
            // the word bolds instead, and the later carets meet it.
            ("xa****b", [0, 4, 6], "**xa****b**", [
                NSRange(location: 2, length: 0), NSRange(location: 6, length: 0),
                NSRange(location: 8, length: 0),
            ]),
            ("a****b", [3, 4], "ab", [NSRange(location: 1, length: 0)]),
        ]
        for (text, carets, expected, selections) in cases {
            let e = edit(in: text, selections: carets.map { NSRange(location: $0, length: 0) })
            for (earlier, later) in zip(e.replacements, e.replacements.dropFirst()) {
                #expect(NSMaxRange(earlier.range) <= later.range.location)
            }
            #expect(applying(e, to: text) == expected)
            #expect(e.selections == selections)
        }
    }

    @Test("a selection of syntax alone changes nothing")
    func syntaxOnly() {
        let e = edit(in: "**a**", selection: NSRange(location: 0, length: 2))
        #expect(e.replacements.isEmpty)
    }

    @Test("wrap then unwrap round-trips")
    func roundTrip() {
        var text = "say word now"
        var e = edit(in: text, selection: NSRange(location: 4, length: 4))
        text = applying(e, to: text)
        #expect(text == "say **word** now")

        e = edit(in: text, selections: e.selections)
        #expect(applying(e, to: text) == "say word now")
        #expect(selected(e, in: text) == ["word"])
    }

    @Test("bold then italic composes into bold italic, and each undoes alone")
    func composes() {
        var text = "word"
        var e = edit(in: text, selection: NSRange(location: 0, length: 4))
        text = applying(e, to: text)
        e = edit(.emphasis, in: text, selections: e.selections)
        text = applying(e, to: text)
        #expect(text == "***word***")
        #expect(e.selections == [NSRange(location: 0, length: 10)])

        // Removing one trait keeps the other.
        let selection = NSRange(location: 0, length: 10)
        #expect(applying(edit(in: text, selection: selection), to: text) == "*word*")
        #expect(
            applying(edit(.emphasis, in: text, selection: selection), to: text)
                == "**word**"
        )
    }

    @Test("styling the end of a span composes, and toggles back")
    func suffixComposes() {
        /// The styles covering `location` in `text`'s display.
        func styles(at location: Int, in text: String) -> [MarkdownStyler.Style] {
            MarkdownStyler.styles(forParagraph: text)
                .filter { $0.style != .marker && NSLocationInRange(location, $0.range) }
                .map(\.style)
        }

        let bold = "**ab**"
        let italic = edit(.emphasis, in: bold, selection: NSRange(location: 3, length: 1))
        let both = applying(italic, to: bold)
        #expect(both == "**a*b***")
        #expect(styles(at: 4, in: both).contains(.strong))
        #expect(styles(at: 4, in: both).contains(.emphasis))
        #expect(applying(edit(.emphasis, in: both, selections: italic.selections), to: both) == bold)

        let emphasized = "*ab*"
        let bolded = applying(
            edit(in: emphasized, selection: NSRange(location: 2, length: 1)), to: emphasized
        )
        #expect(bolded == "*a**b***")
        #expect(styles(at: 4, in: bolded).contains(.strong))
        #expect(styles(at: 4, in: bolded).contains(.emphasis))
    }

    @Test("italicizing a selection with a bold end unwraps cleanly again")
    func mixedSelectionRoundTrip() {
        let text = "_a_ **b**"
        let all = NSRange(location: 0, length: 9)
        let italic = applying(edit(.emphasis, in: text, selection: all), to: text)
        #expect(italic == "*a **b***")

        let again = edit(.emphasis, in: italic, selection: NSRange(location: 0, length: 9))
        #expect(applying(again, to: italic) == "a **b**")
    }

    @Test("a span touching the selection merges instead of colliding")
    func adjacentSpan() {
        // `*a**b*` would leave `**` literal.
        let text = "*a*b"
        let e = edit(.emphasis, in: text, selection: NSRange(location: 3, length: 1))
        let merged = applying(e, to: text)
        #expect(merged == "*ab*")
        #expect(applying(edit(.emphasis, in: merged, selections: e.selections), to: merged) == "ab")
    }

    @Test("a caret in a styled word unwraps it; each caret toggles its word")
    func carets() {
        let span = "**word**"
        let e = edit(in: span, selection: NSRange(location: 4, length: 0))
        #expect(applying(e, to: span) == "word")
        #expect(e.selections == [NSRange(location: 2, length: 0)])

        // At the span's edge, a pair would collide with its markers:
        // the word beside the caret toggles instead.
        let edge = edit(in: span, selection: NSRange(location: 8, length: 0))
        #expect(applying(edge, to: span) == "word")
        #expect(edge.selections == [NSRange(location: 4, length: 0)])

        let text = "ab cd"
        let words = edit(in: text, selections: [
            NSRange(location: 1, length: 0), NSRange(location: 4, length: 0),
        ])
        #expect(applying(words, to: text) == "**ab** **cd**")
        #expect(words.selections == [
            NSRange(location: 3, length: 0), NSRange(location: 10, length: 0),
        ])
    }

    @Test("many spans cost linear time, not quadratic")
    func manySpansStayFast() {
        // Every character checked against every span took seconds.
        let text = String(repeating: "*a* ", count: 16_000)
        let all = NSRange(location: 0, length: (text as NSString).length)
        let clock = ContinuousClock()
        var unwrapped = ""
        let elapsed = clock.measure {
            unwrapped = applying(edit(.emphasis, in: text, selection: all), to: text)
        }
        #expect(unwrapped == String(repeating: "a ", count: 16_000))
        #expect(elapsed < .seconds(10))
    }

    @Test("dissolving a span never breaks the syntax beside it")
    func neighbouringSyntax() {
        /// The styles covering `location` in `text`'s display.
        func styles(at location: Int, in text: String) -> [MarkdownStyler.Style] {
            MarkdownStyler.styles(forParagraph: text)
                .filter { $0.style != .marker && NSLocationInRange(location, $0.range) }
                .map(\.style)
        }

        // Dropping `*a*`'s markers would glue `__` to `a`, so the
        // emphasis stays nested instead — still around the selection,
        // not the whole word `bc`.
        let text = "__x__*a* bc"
        let italic = applying(
            edit(.emphasis, in: text, selection: NSRange(location: 0, length: 10)), to: text
        )
        #expect(italic == "*__x__*a* b*c")
        #expect(styles(at: 3, in: italic).contains(.strong))
        #expect(styles(at: 3, in: italic).contains(.emphasis))

        // Unwrapping both emphases would glue `__` to `a` again: the
        // outer one goes, which is the wrap undone.
        let back = edit(.emphasis, in: italic, selection: NSRange(location: 0, length: 12))
        #expect(applying(back, to: italic) == text)

        // Unbolding `x` here could only leave `__x__`, still bold:
        // nothing changes.
        let tangled = "_**_x_**_"
        #expect(edit(in: tangled, selection: NSRange(location: 4, length: 1)).replacements.isEmpty)
    }

    @Test("growing to the word keeps a combining mark with its space")
    func wordGrowthKeepsCharacters() {
        let text = " \u{0301}hello."
        let e = edit(in: text, selection: NSRange(location: 7, length: 1))
        #expect(applying(e, to: text) == " \u{0301}**hello.**")
    }

    @Test("markers that couldn't open take in the word")
    func flanking() {
        // `hello**.**` wouldn't parse: `**` between a letter and a
        // period can't open.
        let text = "hello."
        let e = edit(in: text, selection: NSRange(location: 5, length: 1))
        let bolded = applying(e, to: text)
        #expect(bolded == "**hello.**")
        #expect(applying(edit(in: bolded, selections: e.selections), to: bolded) == text)
    }

    @Test("bold on an empty marker pair takes it out again")
    func emptyPair() {
        let text = "a****b"
        let e = edit(in: text, selection: NSRange(location: 3, length: 0))
        #expect(applying(e, to: text) == "ab")
        #expect(e.selections == [NSRange(location: 1, length: 0)])
    }

    @Test("a chain of touching spans and deep nesting stay linear")
    func chainsStayFast() {
        let clock = ContinuousClock()

        // Each `*a*` touches the next `_b_`: growing one span per scan
        // was quadratic.
        let chain = String(repeating: "_b_*a*", count: 8_000) + "x"
        let x = NSRange(location: (chain as NSString).length - 1, length: 1)
        var merged = ""
        let chainElapsed = clock.measure {
            merged = applying(edit(.emphasis, in: chain, selection: x), to: chain)
        }
        #expect(merged == "*" + String(repeating: "ba", count: 8_000) + "x*")
        #expect(chainElapsed < .seconds(10))

        // 50,000 strong spans nested around `x`: filling each one's
        // whole range was quadratic.
        let depth = 50_000
        let nested = String(repeating: "**", count: depth) + "x"
            + String(repeating: "**", count: depth)
        let middle = NSRange(location: 2 * depth, length: 1)
        var unwrapped = ""
        let nestedElapsed = clock.measure {
            unwrapped = applying(edit(in: nested, selection: middle), to: nested)
        }
        #expect(unwrapped == "x")
        #expect(nestedElapsed < .seconds(10))
    }

    @Test("many selections map in linear time")
    func manySelectionsStayFast() {
        let count = 24_000
        let text = String(repeating: "a ", count: count)
        let selections = (0 ..< count).map { NSRange(location: 2 * $0, length: 1) }
        let clock = ContinuousClock()
        var e = MarkdownFormatting.Edit(replacements: [], selections: [])
        let elapsed = clock.measure {
            e = edit(in: text, selections: selections)
        }
        #expect(applying(e, to: text) == String(repeating: "**a** ", count: count))
        #expect(e.selections.count == count)
        #expect(e.selections.last == NSRange(location: 6 * (count - 1), length: 5))
        #expect(elapsed < .seconds(10))
    }

    @Test("separate spans in one selection never cross-pair")
    func separateSpans() {
        // Both words bold: ⌘B removes it from each.
        let both = "**a** **b**"
        #expect(
            applying(edit(in: both, selection: NSRange(location: 0, length: 11)), to: both)
                == "a b"
        )
        // One word bold: ⌘B makes the whole selection bold.
        let one = "**a** b"
        #expect(
            applying(edit(in: one, selection: NSRange(location: 0, length: 7)), to: one)
                == "**a b**"
        )
        // A span the selection cuts is taken in whole.
        let cut = "**ab**cd"
        #expect(
            applying(edit(in: cut, selection: NSRange(location: 3, length: 5)), to: cut)
                == "**abcd**"
        )
    }

    @Test("markers stay nested around a span of the other style")
    func nesting() {
        // Bolding `b* cd` takes in the emphasis it would cut.
        let text = "*ab* cd"
        let bolded = applying(
            edit(in: text, selection: NSRange(location: 2, length: 5)), to: text
        )
        #expect(bolded == "***ab* cd**")
        #expect(
            MarkdownStyler.styles(forParagraph: bolded)
                .filter { $0.style != .marker }
                .map(\.style) == [.strong, .emphasis]
        )
    }

    @Test("a selection across paragraphs is formatted per paragraph")
    func acrossParagraphs() {
        // The styler reads one paragraph at a time, so `**one\ntwo**`
        // would style neither line.
        let text = "one\ntwo"
        let e = edit(in: text, selection: NSRange(location: 0, length: 7))
        let bolded = applying(e, to: text)
        #expect(bolded == "**one**\n**two**")
        #expect(e.selections == [NSRange(location: 0, length: 15)])

        #expect(applying(edit(in: bolded, selections: e.selections), to: bolded) == text)
    }

    @Test("every selection keeps its place as earlier ones grow")
    func multipleSelections() {
        let text = "one two"
        let e = edit(in: text, selections: [
            NSRange(location: 0, length: 3), NSRange(location: 4, length: 3),
        ])
        let bolded = applying(e, to: text)
        #expect(bolded == "**one** **two**")
        #expect(e.selections == [
            NSRange(location: 0, length: 7), NSRange(location: 8, length: 7),
        ])

        let back = edit(in: bolded, selections: e.selections)
        #expect(applying(back, to: bolded) == text)
        #expect(back.selections == [
            NSRange(location: 0, length: 3), NSRange(location: 4, length: 3),
        ])
    }

    @Test("markers skip list markers and surrounding whitespace")
    func trims() {
        let item = "- item"
        #expect(
            applying(edit(in: item, selection: NSRange(location: 0, length: 6)), to: item)
                == "- **item**"
        )
        // A closing marker after a space wouldn't close.
        let spaced = "word b"
        #expect(
            applying(edit(in: spaced, selection: NSRange(location: 0, length: 5)), to: spaced)
                == "**word** b"
        )
    }

    @Test("markers never split a surrogate pair")
    func composedCharacters() {
        // Half of 😀 selected: the selection widens to the whole
        // emoji.
        let text = "😀 x"
        let e = edit(in: text, selection: NSRange(location: 0, length: 1))
        #expect(applying(e, to: text) == "**😀** x")
    }

    @Test("markers never split a whitespace-led combining sequence")
    func combiningAfterSpace() {
        // The accent belongs to the space; the markers go around `x`.
        let text = " \u{0301}x"
        let e = edit(in: text, selection: NSRange(location: 0, length: 3))
        #expect(applying(e, to: text) == " \u{0301}**x**")
    }

    @Test("markers around non-ASCII text unwrap too")
    func utf16() {
        let e = edit(in: "**日本**", selection: NSRange(location: 0, length: 6))
        #expect(applying(e, to: "**日本**") == "日本")
        #expect(e.selections == [NSRange(location: 0, length: 2)])
    }
}
