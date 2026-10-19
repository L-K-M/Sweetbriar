// The reader's plain-text view under test (plan M1.5l, spec/08
// "Reader"): TextKit 2 through every operation, the body stored
// verbatim: selectable, uneditable, nothing detected or interpreted —
// quotes styled by depth, and the source mode's monospace. AppKit
// only: macOS CI runs it inside the app bundle.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import Testing

@Suite("the plain-text reader view")
@MainActor
struct PlainTextReaderViewTests {
    /// The view under test, configured exactly as it ships.
    private let view = PlainTextReaderView()

    /// The storage attributes at `location`. Throws when the spot is
    /// missing, so a "not styled" check can't pass on nothing.
    private func attributes(
        at location: Int
    ) throws -> [NSAttributedString.Key: Any] {
        let storage = try #require(view.textStorage)
        try #require(location < storage.length)
        return storage.attributes(at: location, effectiveRange: nil)
    }

    /// Whether the display font at `location` carries `trait`.
    private func displayFont(
        at location: Int,
        has trait: NSFontDescriptor.SymbolicTraits
    ) throws -> Bool {
        let font = try #require(
            attributes(at: location)[.font] as? NSFont
        )
        return font.fontDescriptor.symbolicTraits.contains(trait)
    }

    @Test("the view stays on TextKit 2 through every operation")
    func staysOnTextKit2() {
        #expect(view.isOnTextKit2)

        view.text = "plain\n> quoted"
        #expect(view.isOnTextKit2)
        view.mode = .source
        #expect(view.isOnTextKit2)
        view.mode = .text
        #expect(view.isOnTextKit2)
        view.text = ""
        #expect(view.isOnTextKit2)
    }

    @Test("the body shows literally: verbatim, uneditable, undetected")
    func literalContent() throws {
        let body = "**literal** _markers_\nsee https://example.com/a?b=c\n"
            + "> quoted **not bold** mail@example.com\n"
            + "trailing newline kept:\n"
        view.text = body

        #expect(view.text == body)
        #expect(!view.isEditable)
        #expect(view.isSelectable)
        #expect(!view.isRichText)
        #expect(!view.isAutomaticLinkDetectionEnabled)
        #expect(!view.isAutomaticDataDetectionEnabled)
        #expect(try !displayFont(at: 2, has: .bold))
        #expect(try !displayFont(at: 14, has: .italic))

        // Nothing is a link: literal means nothing detected.
        let storage = try #require(view.textStorage)
        var links = 0
        storage.enumerateAttribute(
            .link,
            in: NSRange(location: 0, length: storage.length)
        ) { value, _, _ in
            if value != nil { links += 1 }
        }
        #expect(links == 0)

        // Selection still works: the reader copies text.
        view.setSelectedRange(NSRange(location: 4, length: 7))
        #expect(view.selectedRange() == NSRange(location: 4, length: 7))
        #expect(view.accessibilityValue() == body)
    }

    @Test("quotes are styled by depth: color, italic, indent")
    func quoteDepthStyling() throws {
        view.text = "top\n> one\n>> two\n>>> three\nend"
        // "top" [0,3), "> one" [4,9), ">> two" [10,16),
        // ">>> three" [17,26), "end" [27,30)

        // Depth 0: the body font, the plain color, no indent.
        #expect(try attributes(at: 1)[.font] as? NSFont
            == PlainTextReaderView.bodyFont)
        #expect(try !displayFont(at: 1, has: .italic))
        #expect(try attributes(at: 1)[.foregroundColor] as? NSColor
            == .textColor)
        #expect(try attributes(at: 1)[.paragraphStyle] == nil)

        // Each quoted line: italic, its depth's color, its depth's
        // indent: deeper indents further.
        for (location, depth) in [(5, 1), (12, 2), (20, 3)] {
            #expect(try displayFont(at: location, has: .italic))
            #expect(try attributes(at: location)[.foregroundColor]
                as? NSColor == PlainTextReaderView.quoteColors[depth - 1])
            let style = try #require(
                attributes(at: location)[.paragraphStyle]
                    as? NSParagraphStyle
            )
            #expect(
                style.headIndent
                    == PlainTextReaderView.quoteIndent * CGFloat(depth)
            )
        }
        #expect(try attributes(at: 5)[.foregroundColor] as? NSColor
            != attributes(at: 12)[.foregroundColor] as? NSColor)

        // Depth 0 again after the quotes.
        #expect(try !displayFont(at: 28, has: .italic))
        #expect(try attributes(at: 28)[.paragraphStyle] == nil)
        #expect(view.isOnTextKit2)
    }

    @Test("source mode is verbatim monospace, styling returns on .text")
    func sourceMode() throws {
        view.text = "top\n> quoted\n"
        view.mode = .source

        // Every line monospaced; the quote's styling is gone.
        #expect(try font(at: 1).isFixedPitch)
        #expect(try font(at: 5).isFixedPitch)
        #expect(try !displayFont(at: 5, has: .italic))
        #expect(try attributes(at: 5)[.foregroundColor] as? NSColor
            == .textColor)
        #expect(try attributes(at: 5)[.paragraphStyle] == nil)
        #expect(view.text == "top\n> quoted\n")

        // Setting text in source mode stays monospace and literal.
        view.text = "new\n>> deep"
        #expect(try font(at: 6).isFixedPitch)
        #expect(try attributes(at: 6)[.paragraphStyle] == nil)

        view.mode = .text
        #expect(try !font(at: 6).isFixedPitch)
        #expect(try displayFont(at: 6, has: .italic))
        #expect(view.isOnTextKit2)
    }

    @Test("blank lines and CRLF preserve quote styling offsets")
    func quotesAfterBlankLines() throws {
        let body = "\r\n\r\nplain 🐦\r\n\r\n> one\r\n\r\n > > two\r\nend"
        view.text = body
        let source = body as NSString
        for (needle, depth) in [("> one", 1), (" > > two", 2)] {
            let range = source.range(of: needle)
            try #require(range.location != NSNotFound)
            let style = try #require(
                attributes(at: range.location)[.paragraphStyle] as? NSParagraphStyle
            )
            #expect(style.headIndent == PlainTextReaderView.quoteIndent * CGFloat(depth))
        }
        #expect(view.string == body)
        #expect(view.isOnTextKit2)
    }

    /// The display font at `location`.
    private func font(at location: Int) throws -> NSFont {
        try #require(attributes(at: location)[.font] as? NSFont)
    }
}
#endif
