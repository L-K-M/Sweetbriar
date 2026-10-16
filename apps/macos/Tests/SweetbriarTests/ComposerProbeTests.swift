// The Composer Probe's editor under test (plan M0.11): ⌘B as one
// undoable edit that restores text and styling together, marked text
// left unstyled until it commits, TextKit 2 staying on through every
// operation, and the accessibility value being the text. AppKit only —
// macOS CI runs it inside the app bundle.

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
    /// the display layer was told — styled or untouched.
    private func paragraphs() -> [NSTextParagraph] {
        guard let layoutManager = textView.textLayoutManager,
              let storage = layoutManager.textContentManager
                  as? NSTextContentStorage else {
            return []
        }
        layoutManager.ensureLayout(for: layoutManager.documentRange)
        return storage.textElements(for: layoutManager.documentRange)
            .compactMap { $0 as? NSTextParagraph }
    }

    /// The display attributes at `location` in the document's first
    /// paragraph — empty when the element or the attribute is absent.
    private func displayAttributes(
        at location: Int
    ) -> [NSAttributedString.Key: Any] {
        guard let paragraph = paragraphs().first,
              location < paragraph.attributedString.length else {
            return [:]
        }
        return paragraph.attributedString.attributes(
            at: location, effectiveRange: nil
        )
    }

    @Test("⌘B wraps the selection in one undoable edit")
    func boldWrapUndo() {
        textView.mode = .markdown
        textView.string = "say word now"
        textView.setSelectedRange(NSRange(location: 4, length: 4))

        textView.toggleBoldMarkdown(nil)
        #expect(textView.string == "say **word** now")
        #expect(textView.isOnTextKit2)

        // The markers dim, the word goes bold — presentation only,
        // never in the stored text.
        let markerColor = displayAttributes(at: 4)[.foregroundColor]
            as? NSColor
        #expect(markerColor == .tertiaryLabelColor)
        let wordFont = displayAttributes(at: 6)[.font] as? NSFont
        #expect(wordFont?.fontDescriptor.symbolicTraits.contains(.bold) == true)

        // One undo restores the text…
        #expect(textView.undoManager?.canUndo == true)
        textView.undoManager?.undo()
        #expect(textView.string == "say word now")
        #expect(textView.undoManager?.canRedo == true)

        // …and the styling with it: the styler's runs match the
        // restored text, and the paragraph is unstyled again.
        #expect(MarkdownStyler.styles(forParagraph: textView.string).isEmpty)
        let restored = displayAttributes(at: 4)
        #expect(restored[.foregroundColor] as? NSColor != .tertiaryLabelColor)
        let restoredFont = restored[.font] as? NSFont
        #expect(restoredFont?.fontDescriptor.symbolicTraits.contains(.bold) != true)
        #expect(textView.isOnTextKit2)
    }

    @Test("marked text stays unstyled until it commits")
    func markedText() {
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
        let during = displayAttributes(at: 0)
        #expect(during[.foregroundColor] as? NSColor != .tertiaryLabelColor)
        #expect(textView.isOnTextKit2)

        textView.insertText(
            "日本",
            replacementRange: NSRange(location: markedLocation, length: 2)
        )
        #expect(textView.string == "**x** 日本")
        #expect(!textView.hasMarkedText())

        // Committed: the delegate styles the paragraph again.
        let after = displayAttributes(at: 0)
        #expect(after[.foregroundColor] as? NSColor == .tertiaryLabelColor)
        #expect(textView.isOnTextKit2)
    }

    @Test("the view stays on TextKit 2 through every operation")
    func staysOnTextKit2() {
        #expect(textView.isOnTextKit2)

        textView.insertText(
            "word", replacementRange: NSRange(location: 0, length: 0)
        )
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
    func plainMode() {
        textView.string = "**word**"
        textView.mode = .plain

        let item = NSMenuItem(
            title: "Bold",
            action: #selector(ComposerTextView.toggleBoldMarkdown(_:)),
            keyEquivalent: "b"
        )
        #expect(!textView.validateUserInterfaceItem(item))
        #expect(
            displayAttributes(at: 0)[.foregroundColor] as? NSColor
                != .tertiaryLabelColor
        )

        textView.mode = .markdown
        #expect(textView.validateUserInterfaceItem(item))
        #expect(
            displayAttributes(at: 0)[.foregroundColor] as? NSColor
                == .tertiaryLabelColor
        )
        #expect(textView.isOnTextKit2)
    }

    @Test("the accessibility value is the text")
    func accessibilityValue() {
        textView.string = "hello **world**"
        #expect(textView.accessibilityLabel() == "Message body")
        #expect(textView.accessibilityValue() as? String == "hello **world**")
    }
}
#endif
