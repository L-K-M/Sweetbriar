import AppKit
import SweetbriarBridge

enum ReadingRenderer {
    static func attributedString(for document: ReadingDocument) -> NSAttributedString {
        let text = NSMutableAttributedString()
        for block in document.blocks { render(block, depth: 0, into: text) }
        return text
    }

    private static func render(_ block: ReadingBlock, depth: Int, into text: NSMutableAttributedString) {
        switch block {
        case .paragraph(_, let inlines):
            render(inlines, into: text)
            text.append(NSAttributedString(string: "\n\n"))
        case .heading(_, let level, let inlines):
            let start = text.length
            render(inlines, into: text)
            text.addAttributes(
                headingAttributes(level: level),
                range: NSRange(location: start, length: text.length - start)
            )
            text.append(NSAttributedString(string: "\n\n"))
        case .quote(_, let blocks):
            // Each child is styled once, at its own depth: a nested quote
            // styles itself, so the outer quote skips it.
            for block in blocks {
                let start = text.length
                render(block, depth: depth + 1, into: text)
                if case .quote = block { continue }
                quote(NSRange(location: start, length: text.length - start), depth: depth + 1, in: text)
            }
        case .list(_, let ordered, let start, let reversed, let items):
            render(items, ordered: ordered, start: start, reversed: reversed, into: text)
        case .preformatted(_, let preformatted):
            let start = text.length
            text.append(NSAttributedString(string: preformatted))
            text.addAttributes(
                [.font: NSFont.monospacedSystemFont(ofSize: NSFont.systemFontSize, weight: .regular)],
                range: NSRange(location: start, length: text.length - start)
            )
            text.append(NSAttributedString(string: "\n\n"))
        }
    }

    private static func render(_ inlines: [ReadingInline], into text: NSMutableAttributedString) {
        for inline in inlines {
            switch inline {
            case .text(_, let value):
                text.append(NSAttributedString(string: value, attributes: [.font: QuoteDepth.bodyFont]))
            case .lineBreak:
                text.append(NSAttributedString(string: "\n"))
            case .emphasis(_, let strong, let inlines):
                let start = text.length
                render(inlines, into: text)
                add(
                    strong ? .boldFontMask : .italicFontMask,
                    to: NSRange(location: start, length: text.length - start), in: text)
            case .link(_, let destination, let inlines):
                let start = text.length
                render(inlines, into: text)
                var attributes: [NSAttributedString.Key: Any] = [.foregroundColor: NSColor.linkColor]
                if let url = URL(string: destination) { attributes[.link] = url }
                text.addAttributes(attributes, range: NSRange(location: start, length: text.length - start))
            case .image(_, let alt, _):
                let value = alt.isEmpty ? "Image" : alt
                text.append(NSAttributedString(string: value, attributes: [.font: QuoteDepth.bodyFont]))
            }
        }
    }

    private static func render(
        _ items: [ReadingListItem], ordered: Bool, start: Int64, reversed: Bool,
        into text: NSMutableAttributedString
    ) {
        var next = start
        for item in items {
            let number = item.number ?? next
            let content = NSMutableAttributedString()
            for block in item.blocks { render(block, depth: 0, into: content) }
            while content.length > 0, content.string.hasSuffix("\n") {
                content.deleteCharacters(in: NSRange(location: content.length - 1, length: 1))
            }
            let itemStart = text.length
            text.append(NSAttributedString(string: ordered ? "\(number). " : "• "))
            text.append(content)
            text.append(NSAttributedString(string: "\n"))
            indent(NSRange(location: itemStart, length: text.length - itemStart), by: 24, in: text)
            if ordered {
                next = advance(number, reversed: reversed)
            }
        }
    }

    /// The following implicit number. HTML ordinal values are conceptually
    /// unbounded; the document model carries Int64, so pinning at its edge
    /// keeps hostile input from trapping while ordinary sequences count on.
    private static func advance(_ number: Int64, reversed: Bool) -> Int64 {
        let (value, overflow) = reversed
            ? number.subtractingReportingOverflow(1)
            : number.addingReportingOverflow(1)
        return overflow ? (reversed ? Int64.min : Int64.max) : value
    }

    /// A quote's color and indent at `depth` (spec/08, shared with the
    /// plain-text view), with italics added to the fonts already there,
    /// so headings, emphasis and code inside it keep their own style and
    /// links keep the link color.
    private static func quote(_ range: NSRange, depth: Int, in text: NSMutableAttributedString) {
        let attributes = QuoteDepth.attributes(forDepth: depth)
        let style = attributes[.paragraphStyle] as? NSParagraphStyle
        indent(range, by: style?.headIndent ?? 0, in: text)
        text.enumerateAttribute(.link, in: range) { link, subrange, _ in
            guard link == nil else { return }
            text.addAttribute(.foregroundColor, value: attributes[.foregroundColor] as Any, range: subrange)
        }
        add(.italicFontMask, to: range, in: text)
    }

    /// Adds `points` to the indent of every paragraph in `range`, on top
    /// of the indent its content already has (a list in a quote, a quote
    /// or nested list in a list item) rather than replacing it.
    private static func indent(_ range: NSRange, by points: CGFloat, in text: NSMutableAttributedString) {
        text.enumerateAttribute(.paragraphStyle, in: range) { style, subrange, _ in
            let paragraph = NSMutableParagraphStyle()
            if let style = style as? NSParagraphStyle { paragraph.setParagraphStyle(style) }
            paragraph.firstLineHeadIndent += points
            paragraph.headIndent += points
            text.addAttribute(.paragraphStyle, value: paragraph, range: subrange)
        }
    }

    /// Adds `trait` to every font in `range`, keeping the traits and
    /// sizes already set there.
    private static func add(_ trait: NSFontTraitMask, to range: NSRange, in text: NSMutableAttributedString) {
        text.enumerateAttribute(.font, in: range) { font, subrange, _ in
            let current = font as? NSFont ?? QuoteDepth.bodyFont
            text.addAttribute(
                .font, value: NSFontManager.shared.convert(current, toHaveTrait: trait), range: subrange)
        }
    }

    private static func headingAttributes(level: ReadingHeadingLevel) -> [NSAttributedString.Key: Any] {
        let sizes: [ReadingHeadingLevel: CGFloat] = [.h1: 24, .h2: 20, .h3: 17, .h4: 15, .h5: 13, .h6: 12]
        return [
            .font: NSFontManager.shared.convert(
                NSFont.systemFont(ofSize: sizes[level] ?? NSFont.systemFontSize),
                toHaveTrait: .boldFontMask
            )
        ]
    }
}
