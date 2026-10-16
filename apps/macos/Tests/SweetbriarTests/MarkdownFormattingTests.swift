// The composer's ⌘B/⌘I edit under test (plan M0.11): wrap, unwrap
// with the markers inside or just outside the selection, the empty
// selection's marker pair, and the round trip. Foundation only, so
// it runs on Linux too.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
import Foundation
import Testing

@Suite("the Markdown formatting edit")
struct MarkdownFormattingTests {
    /// The edit `marker` computes for `selection` in `text`.
    private func edit(
        _ marker: MarkdownFormatting.Marker = .strong,
        in text: String,
        selection: NSRange
    ) -> MarkdownFormatting.Edit {
        MarkdownFormatting.edit(for: marker, in: text, selection: selection)
    }

    /// The string after applying `edit` — what the text storage
    /// would hold.
    private func applying(_ edit: MarkdownFormatting.Edit, to text: String) -> String {
        (text as NSString).replacingCharacters(in: edit.range, with: edit.replacement)
    }

    @Test("wrapping a selection covers the wrapped span")
    func wrap() {
        let e = edit(in: "a word c", selection: NSRange(location: 2, length: 4))
        #expect(e.range == NSRange(location: 2, length: 4))
        #expect(e.replacement == "**word**")

        let result = applying(e, to: "a word c")
        #expect(result == "a **word** c")
        // The new selection covers `**word**`, so ⌘B again unwraps.
        #expect((result as NSString).substring(with: e.selection) == "**word**")
    }

    @Test("emphasis wraps in a single star")
    func emphasisWrap() {
        let e = edit(.emphasis, in: "word", selection: NSRange(location: 0, length: 4))
        #expect(e.replacement == "*word*")
        #expect(e.selection == NSRange(location: 0, length: 6))
    }

    @Test("unwrapping markers inside the selection")
    func unwrapInside() {
        let e = edit(in: "a **word** c", selection: NSRange(location: 2, length: 8))
        #expect(e.range == NSRange(location: 2, length: 8))
        #expect(e.replacement == "word")
        #expect(e.selection == NSRange(location: 2, length: 4))
        #expect(applying(e, to: "a **word** c") == "a word c")

        // Emphasis drops the outer star of `**…**`, one layer at a
        // time.
        let i = edit(.emphasis, in: "**word**", selection: NSRange(location: 0, length: 8))
        #expect(i.replacement == "*word*")
    }

    @Test("unwrapping markers just outside the selection")
    func unwrapOutside() {
        let e = edit(in: "a **word** c", selection: NSRange(location: 4, length: 4))
        #expect(e.range == NSRange(location: 2, length: 8))
        #expect(e.replacement == "word")
        #expect(applying(e, to: "a **word** c") == "a word c")
        // The selection follows the word the dropped markers left.
        #expect(e.selection == NSRange(location: 2, length: 4))

        let i = edit(.emphasis, in: "a *word* c", selection: NSRange(location: 3, length: 4))
        #expect(applying(i, to: "a *word* c") == "a word c")
    }

    @Test("an empty selection inserts a marker pair with the caret between")
    func emptySelection() {
        let e = edit(in: "ab", selection: NSRange(location: 1, length: 0))
        #expect(e.replacement == "****")
        #expect(e.selection == NSRange(location: 3, length: 0))
        #expect(applying(e, to: "ab") == "a****b")

        let i = edit(.emphasis, in: "ab", selection: NSRange(location: 1, length: 0))
        #expect(i.replacement == "**")
        #expect(i.selection == NSRange(location: 2, length: 0))
    }

    @Test("wrap then unwrap round-trips")
    func roundTrip() {
        var text = "say word now"
        var e = edit(in: text, selection: NSRange(location: 4, length: 4))
        text = applying(e, to: text)
        #expect(text == "say **word** now")

        e = edit(in: text, selection: e.selection)
        text = applying(e, to: text)
        #expect(text == "say word now")
        #expect((text as NSString).substring(with: e.selection) == "word")
    }

    @Test("markers around non-ASCII text unwrap too")
    func utf16() {
        let e = edit(in: "**日本**", selection: NSRange(location: 0, length: 6))
        #expect(e.replacement == "日本")
        #expect(e.selection == NSRange(location: 0, length: 2))
    }
}
