import AppKit
import Foundation
import SweetbriarBridge
import Testing

@testable import Sweetbriar

@Suite @MainActor
struct ReadingRendererTests {
    private func paragraph(_ inlines: [ReadingInline]) -> ReadingBlock {
        .paragraph(partID: "p", inlines: inlines)
    }

    private func literal(_ value: String) -> ReadingInline {
        .text(partID: "p", text: value)
    }

    private func render(_ blocks: [ReadingBlock]) -> NSAttributedString {
        ReadingRenderer.attributedString(for: .init(sourcePartID: "p", version: 1, blocks: blocks))
    }

    private func text(_ blocks: [ReadingBlock]) -> String {
        render(blocks).string
    }

    private func attribute(
        _ blocks: [ReadingBlock], _ key: NSAttributedString.Key, at location: Int
    ) throws -> Any? {
        let attributed = render(blocks)
        try #require(location < attributed.length)
        return attributed.attributes(at: location, effectiveRange: nil)[key]
    }

    private func font(_ blocks: [ReadingBlock], at location: Int) throws -> NSFont {
        try #require(attribute(blocks, .font, at: location) as? NSFont)
    }

    @Test func paragraphRendersBodyText() throws {
        #expect(text([paragraph([literal("Hello")])]) == "Hello\n\n")
        #expect(try font([paragraph([literal("Hello")])], at: 0) == QuoteDepth.bodyFont)
    }

    @Test func headingRendersBoldByLevel() throws {
        let blocks: [ReadingBlock] = [.heading(partID: "p", level: .h2, inlines: [literal("Title")])]
        #expect(text(blocks) == "Title\n\n")
        #expect(try font(blocks, at: 0).fontDescriptor.symbolicTraits.contains(.bold))
    }

    @Test func quoteIsStyledByDepth() throws {
        let blocks: [ReadingBlock] = [.quote(partID: "p", blocks: [paragraph([literal("Quoted")])])]
        #expect(try attribute(blocks, .foregroundColor, at: 0) as? NSColor == QuoteDepth.colors[0])
        let style = try #require(attribute(blocks, .paragraphStyle, at: 0) as? NSParagraphStyle)
        #expect(style.headIndent == QuoteDepth.indent)
    }

    @Test func nestedQuotesKeepTheirOwnDepth() throws {
        let inner: ReadingBlock = .quote(partID: "p", blocks: [paragraph([literal("Inner")])])
        let blocks: [ReadingBlock] = [.quote(partID: "p", blocks: [paragraph([literal("Outer")]), inner])]
        let rendered = render(blocks)
        let innerAt = (rendered.string as NSString).range(of: "Inner").location
        let color = rendered.attributes(at: innerAt, effectiveRange: nil)[.foregroundColor] as? NSColor
        #expect(color == QuoteDepth.colors[1])
        let style = try #require(
            rendered.attributes(at: innerAt, effectiveRange: nil)[.paragraphStyle] as? NSParagraphStyle)
        #expect(style.headIndent == QuoteDepth.indent * 2)
    }

    @Test func quotesKeepTheStylesInsideThem() throws {
        let blocks: [ReadingBlock] = [
            .quote(
                partID: "p",
                blocks: [
                    .heading(partID: "p", level: .h2, inlines: [literal("Title")]),
                    paragraph([
                        .link(partID: "p", destination: "https://example.test/a", inlines: [literal("here")])
                    ]),
                ])
        ]
        let rendered = render(blocks)
        let heading = try #require(rendered.attributes(at: 0, effectiveRange: nil)[.font] as? NSFont)
        #expect(heading.fontDescriptor.symbolicTraits.contains(.bold))
        #expect(heading.fontDescriptor.symbolicTraits.contains(.italic))
        let linkAt = (rendered.string as NSString).range(of: "here").location
        #expect(
            rendered.attributes(at: linkAt, effectiveRange: nil)[.foregroundColor] as? NSColor == .linkColor)
    }

    @Test func strongInsideEmphasisIsBoldAndItalic() throws {
        let blocks: [ReadingBlock] = [
            paragraph([
                .emphasis(
                    partID: "p", strong: false,
                    inlines: [.emphasis(partID: "p", strong: true, inlines: [literal("both")])])
            ])
        ]
        let traits = try font(blocks, at: 0).fontDescriptor.symbolicTraits
        #expect(traits.contains(.bold))
        #expect(traits.contains(.italic))
    }

    @Test func listsRenderNumbersOrBullets() throws {
        let item = ReadingListItem(partID: "p", number: nil, blocks: [paragraph([literal("One")])])
        #expect(
            text([.list(partID: "p", ordered: true, start: 1, reversed: false, items: [item])]) == "1. One\n")
        #expect(
            text([.list(partID: "p", ordered: false, start: 1, reversed: false, items: [item])]) == "• One\n")
    }

    @Test func explicitListNumbersResetTheFollowingSequence() {
        func item(_ text: String, number: Int64? = nil) -> ReadingListItem {
            ReadingListItem(partID: "p", number: number, blocks: [paragraph([literal(text)])])
        }
        let forward: ReadingBlock = .list(
            partID: "p", ordered: true, start: 4, reversed: false,
            items: [item("four"), item("nine", number: 9), item("ten")])
        #expect(text([forward]) == "4. four\n9. nine\n10. ten\n")

        let backward: ReadingBlock = .list(
            partID: "p", ordered: true, start: 3, reversed: true,
            items: [item("three"), item("ten", number: 10), item("nine")])
        #expect(text([backward]) == "3. three\n10. ten\n9. nine\n")
    }

    @Test func listNumberingSaturatesAtIntegerLimits() {
        func item(_ text: String) -> ReadingListItem {
            ReadingListItem(partID: "p", number: nil, blocks: [paragraph([literal(text)])])
        }
        let high: ReadingBlock = .list(
            partID: "p", ordered: true, start: Int64.max, reversed: false,
            items: [item("first"), item("second")])
        #expect(text([high]) == "\(Int64.max). first\n\(Int64.max). second\n")
        let low: ReadingBlock = .list(
            partID: "p", ordered: true, start: Int64.min, reversed: true,
            items: [item("first"), item("second")])
        #expect(text([low]) == "\(Int64.min). first\n\(Int64.min). second\n")
    }

    @Test func listItemsIndentOnTopOfTheirContent() throws {
        func list(_ blocks: [ReadingBlock]) -> ReadingBlock {
            .list(
                partID: "p", ordered: false, start: 1, reversed: false,
                items: [ReadingListItem(partID: "p", number: nil, blocks: blocks)])
        }
        func indent(of word: String, in blocks: [ReadingBlock]) throws -> CGFloat {
            let rendered = render(blocks)
            let at = (rendered.string as NSString).range(of: word).location
            return try #require(
                rendered.attributes(at: at, effectiveRange: nil)[.paragraphStyle] as? NSParagraphStyle
            ).headIndent
        }
        let quoted = list([.quote(partID: "p", blocks: [paragraph([literal("Quoted")])])])
        #expect(try indent(of: "Quoted", in: [quoted]) == 24 + QuoteDepth.indent)
        let nested = list([list([paragraph([literal("Inner")])])])
        #expect(try indent(of: "Inner", in: [nested]) == 48)
        let listInQuote: ReadingBlock = .quote(
            partID: "p", blocks: [paragraph([literal("Plain")]), list([paragraph([literal("Item")])])])
        #expect(try indent(of: "Plain", in: [listInQuote]) == QuoteDepth.indent)
        #expect(try indent(of: "Item", in: [listInQuote]) == QuoteDepth.indent + 24)
    }

    @Test func preformattedIsMonospaced() throws {
        #expect(try font([.preformatted(partID: "p", text: "code")], at: 0).isFixedPitch)
    }

    @Test func emphasisAndStrongStyleFonts() throws {
        let blocks: [ReadingBlock] = [
            paragraph([
                .emphasis(partID: "p", strong: false, inlines: [literal("em")]),
                literal(" "),
                .emphasis(partID: "p", strong: true, inlines: [literal("strong")]),
            ])
        ]
        #expect(try font(blocks, at: 0).fontDescriptor.symbolicTraits.contains(.italic))
        #expect(try font(blocks, at: 4).fontDescriptor.symbolicTraits.contains(.bold))
    }

    @Test func lineBreakRendersANewline() throws {
        #expect(text([paragraph([literal("a"), .lineBreak(partID: "p"), literal("b")])]) == "a\nb\n\n")
    }

    @Test func linkIsClickableWithItsDestination() throws {
        let blocks: [ReadingBlock] = [
            paragraph([
                .link(partID: "p", destination: "https://example.test/a", inlines: [literal("here")])
            ])
        ]
        #expect(try attribute(blocks, .link, at: 0) as? URL == URL(string: "https://example.test/a"))
        #expect(text(blocks) == "here\n\n")
    }

    @Test func imageShowsAltTextOrImage() throws {
        let alt = ReadingInline.image(
            partID: "p", alt: "A bird", reference: .remote(url: "https://example.test/b.png"))
        let missing = ReadingInline.image(
            partID: "p", alt: "", reference: .remote(url: "https://example.test/c.png"))
        #expect(text([paragraph([alt])]) == "A bird\n\n")
        #expect(text([paragraph([missing])]) == "Image\n\n")
    }
}
