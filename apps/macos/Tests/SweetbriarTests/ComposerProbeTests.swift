// The Composer Probe's editor under test (plan M0.11): ⌘B as one
// undoable edit that restores text and styling together — across
// paragraphs and selections too — bold and italic composing, marked
// text left unstyled until it commits, TextKit 2 staying on through
// every operation, and the accessibility value being the text. AppKit
// only — macOS CI runs it inside the app bundle.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import Testing

@Suite("the composer probe editor")
@MainActor
struct ComposerProbeTests {
    /// The probe window, held for the test's life so the editor's
    /// window — offscreen but real — can't drop out from under it.
    private let controller = ComposerProbeWindowController()

    /// The editor under test, configured exactly as it ships.
    private var textView: ComposerTextView {
        controller.textView
    }

    /// The paragraphs TextKit 2 generated for the document: asking
    /// for the elements runs the delegate, so each answer shows what
    /// the display layer was told — styled or untouched. Enumerated
    /// rather than read through `textElements(for:)`, which can come
    /// back empty.
    private func paragraphs() -> [NSTextParagraph] {
        guard let storage = textView.textLayoutManager?.textContentManager
            as? NSTextContentStorage else {
            return []
        }
        var paragraphs: [NSTextParagraph] = []
        _ = storage.enumerateTextElements(from: storage.documentRange.location) {
            if let paragraph = $0 as? NSTextParagraph {
                paragraphs.append(paragraph)
            }
            return true
        }
        return paragraphs
    }

    /// The display attributes at `location` in paragraph `index`.
    /// Throws when the paragraph or the location is missing, so a
    /// "not styled" check can't pass on nothing.
    private func displayAttributes(
        at location: Int,
        inParagraph index: Int = 0
    ) throws -> [NSAttributedString.Key: Any] {
        let all = paragraphs()
        let paragraph = try #require(index < all.count ? all[index] : nil)
        try #require(location < paragraph.attributedString.length)
        return paragraph.attributedString.attributes(
            at: location, effectiveRange: nil
        )
    }

    /// Close the undo group the current event opened. In the app each
    /// key event ends its own; a test's actions share one, so a
    /// single undo would take back every edit before it.
    private func endEvent() {
        guard let undoManager = textView.undoManager,
              undoManager.groupingLevel > 0 else {
            return
        }
        undoManager.endUndoGrouping()
    }

    /// Whether the display font at `location` carries `trait`.
    private func displayFont(
        at location: Int,
        inParagraph index: Int = 0,
        has trait: NSFontDescriptor.SymbolicTraits
    ) throws -> Bool {
        let font = try displayAttributes(at: location, inParagraph: index)[.font]
            as? NSFont
        return font?.fontDescriptor.symbolicTraits.contains(trait) == true
    }

    @Test("⌘B wraps the selection in one undoable edit")
    func boldWrapUndo() throws {
        textView.mode = .markdown
        textView.string = "say word now"
        textView.setSelectedRange(NSRange(location: 4, length: 4))

        textView.toggleBoldMarkdown(nil)
        #expect(textView.string == "say **word** now")
        #expect(textView.isOnTextKit2)

        // The markers dim, the word goes bold — presentation only,
        // never in the stored text.
        let markerColor = try displayAttributes(at: 4)[.foregroundColor]
            as? NSColor
        #expect(markerColor == .tertiaryLabelColor)
        #expect(try displayFont(at: 6, has: .bold))

        // One undo restores the text…
        #expect(textView.undoManager?.canUndo == true)
        textView.undoManager?.undo()
        #expect(textView.string == "say word now")
        #expect(textView.undoManager?.canRedo == true)

        // …and the styling with it: the styler's runs match the
        // restored text, and the paragraph is unstyled again.
        #expect(MarkdownStyler.styles(forParagraph: textView.string).isEmpty)
        let restored = try displayAttributes(at: 4)
        #expect(restored[.foregroundColor] as? NSColor != .tertiaryLabelColor)
        #expect(try !displayFont(at: 4, has: .bold))
        #expect(textView.isOnTextKit2)
    }

    @Test("⌘B across paragraphs bolds each, and one undo restores both")
    func boldAcrossParagraphs() throws {
        textView.mode = .markdown
        textView.string = "one\ntwo"
        textView.setSelectedRange(NSRange(location: 0, length: 7))

        textView.toggleBoldMarkdown(nil)
        #expect(textView.string == "**one**\n**two**")
        #expect(try displayFont(at: 3, inParagraph: 0, has: .bold))
        #expect(try displayFont(at: 3, inParagraph: 1, has: .bold))

        textView.undoManager?.undo()
        #expect(textView.string == "one\ntwo")
        #expect(try !displayFont(at: 1, inParagraph: 0, has: .bold))
        #expect(try !displayFont(at: 1, inParagraph: 1, has: .bold))
    }

    @Test("⌘B over two selections keeps each on its word")
    func boldTwoSelections() {
        textView.mode = .markdown
        textView.string = "one two"
        textView.selectedRanges = [
            NSValue(range: NSRange(location: 0, length: 3)),
            NSValue(range: NSRange(location: 4, length: 3)),
        ]

        textView.toggleBoldMarkdown(nil)
        #expect(textView.string == "**one** **two**")
        #expect(textView.selectedRanges.map(\.rangeValue) == [
            NSRange(location: 0, length: 7), NSRange(location: 8, length: 7),
        ])

        // The same command on the same selections toggles back.
        textView.toggleBoldMarkdown(nil)
        #expect(textView.string == "one two")
    }

    @Test("⌘B then ⌘I shows bold italic")
    func boldThenItalic() throws {
        textView.mode = .markdown
        textView.string = "word"
        textView.setSelectedRange(NSRange(location: 0, length: 4))

        textView.toggleBoldMarkdown(nil)
        textView.toggleItalicMarkdown(nil)
        #expect(textView.string == "***word***")
        #expect(try displayFont(at: 4, has: .bold))
        #expect(try displayFont(at: 4, has: .italic))
    }

    @Test("marked text stays unstyled until it commits")
    func markedText() throws {
        textView.mode = .markdown
        textView.string = "**x** "
        let markedLocation = "**x** ".utf16.count

        textView.setMarkedText(
            "にほ",
            selectedRange: NSRange(location: 0, length: 2),
            replacementRange: NSRange(location: markedLocation, length: 0)
        )
        #expect(textView.hasMarkedText())

        // While marked, the paragraph's element is the unstyled
        // default — the delegate returned nil.
        let during = try displayAttributes(at: 0)
        #expect(during[.foregroundColor] as? NSColor != .tertiaryLabelColor)
        #expect(textView.isOnTextKit2)

        textView.insertText(
            "日本",
            replacementRange: NSRange(location: markedLocation, length: 2)
        )
        #expect(textView.string == "**x** 日本")
        #expect(!textView.hasMarkedText())

        // Committed: the delegate styles the paragraph again.
        let after = try displayAttributes(at: 0)
        #expect(after[.foregroundColor] as? NSColor == .tertiaryLabelColor)
        #expect(textView.isOnTextKit2)
    }

    @Test("cancelling a composition restyles its paragraph")
    func cancelledComposition() throws {
        textView.mode = .markdown
        textView.string = "**x** "
        textView.setMarkedText(
            "に",
            selectedRange: NSRange(location: 1, length: 0),
            replacementRange: NSRange(location: 6, length: 0)
        )
        #expect(
            try displayAttributes(at: 0)[.foregroundColor] as? NSColor
                != .tertiaryLabelColor
        )

        // An input method cancels by marking nothing.
        textView.setMarkedText(
            "",
            selectedRange: NSRange(location: 0, length: 0),
            replacementRange: NSRange(location: NSNotFound, length: 0)
        )
        #expect(!textView.hasMarkedText())
        #expect(textView.string == "**x** ")
        #expect(
            try displayAttributes(at: 0)[.foregroundColor] as? NSColor
                == .tertiaryLabelColor
        )
    }

    @Test("the view stays on TextKit 2 through every operation")
    func staysOnTextKit2() {
        textView.mode = .markdown
        #expect(textView.isOnTextKit2)

        textView.insertText(
            "word", replacementRange: NSRange(location: 0, length: 0)
        )
        endEvent()
        #expect(textView.isOnTextKit2)

        textView.setSelectedRange(NSRange(location: 0, length: 4))
        textView.toggleBoldMarkdown(nil)
        #expect(textView.string == "**word**")
        #expect(textView.isOnTextKit2)

        textView.undoManager?.undo()
        #expect(textView.isOnTextKit2)

        textView.setMarkedText(
            "に",
            selectedRange: NSRange(location: 0, length: 1),
            replacementRange: NSRange(location: 4, length: 0)
        )
        textView.insertText(
            "に", replacementRange: NSRange(location: 4, length: 1)
        )
        #expect(textView.string == "wordに")
        #expect(textView.isOnTextKit2)

        textView.mode = .plain
        #expect(textView.isOnTextKit2)
        textView.mode = .markdown
        #expect(textView.isOnTextKit2)
    }

    @Test("plain mode leaves text unstyled and turns the commands off")
    func plainMode() throws {
        textView.string = "**word**"
        textView.mode = .plain

        let item = NSMenuItem(
            title: "Bold",
            action: #selector(ComposerTextView.toggleBoldMarkdown(_:)),
            keyEquivalent: "b"
        )
        #expect(!textView.validateUserInterfaceItem(item))
        #expect(
            try displayAttributes(at: 0)[.foregroundColor] as? NSColor
                != .tertiaryLabelColor
        )

        textView.mode = .markdown
        #expect(textView.validateUserInterfaceItem(item))
        #expect(
            try displayAttributes(at: 0)[.foregroundColor] as? NSColor
                == .tertiaryLabelColor
        )
        #expect(textView.isOnTextKit2)
    }

    @Test("the accessibility value is the text")
    func accessibilityValue() {
        textView.string = "hello **world**"
        #expect(textView.accessibilityLabel() == "Message body")
        #expect(textView.accessibilityValue() == "hello **world**")
    }
}
#endif
