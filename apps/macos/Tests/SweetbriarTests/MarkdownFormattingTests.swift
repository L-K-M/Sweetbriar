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
    /// would hold. Last to first, as the view applies them.
    private func applying(_ edit: MarkdownFormatting.Edit, to text: String) -> String {
        var result = text as NSString
        for replacement in edit.replacements.reversed() {
            result = result.replacingCharacters(
                in: replacement.range, with: replacement.string
            ) as NSString
        }
        return result as String
    }

    /// The text `edit` leaves selected.
    private func selected(_ edit: MarkdownFormatting.Edit, in text: String) -> [String] {
        let result = applying(edit, to: text) as NSString
        return edit.selections.map { result.substring(with: $0) }
    }

    @Test("wrapping a selection covers the wrapped span")
    func wrap() {
        let e = edit(in: "a word c", selection: NSRange(location: 2, length: 4))
        #expect(e.replacements == [MarkdownFormatting.Replacement(
            range: NSRange(location: 2, length: 4), string: "**word**"
        )])
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

    @Test("an empty selection inserts a marker pair with the caret between")
    func emptySelection() {
        let e = edit(in: "ab", selection: NSRange(location: 1, length: 0))
        #expect(applying(e, to: "ab") == "a****b")
        #expect(e.selections == [NSRange(location: 3, length: 0)])

        let i = edit(.emphasis, in: "ab", selection: NSRange(location: 1, length: 0))
        #expect(applying(i, to: "ab") == "a**b")
        #expect(i.selections == [NSRange(location: 2, length: 0)])
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

    @Test("markers around non-ASCII text unwrap too")
    func utf16() {
        let e = edit(in: "**日本**", selection: NSRange(location: 0, length: 6))
        #expect(applying(e, to: "**日本**") == "日本")
        #expect(e.selections == [NSRange(location: 0, length: 2)])
    }
}
