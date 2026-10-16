// The composer styler's runs under test (plan M0.11): every
// construct — headings, strong, emphasis, lists — plus nesting,
// unclosed and intraword markers, indent limits, and UTF-16 ranges
// around non-ASCII text. Foundation only, so it runs on Linux too.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
import Foundation
import Testing

@Suite("the Markdown styler")
struct MarkdownStylerTests {
    /// The runs of one paragraph, shorthand.
    private func runs(_ text: String) -> [MarkdownStyler.Run] {
        MarkdownStyler.styles(forParagraph: text)
    }

    /// Just the styles, in order — when the exact ranges don't
    /// matter.
    private func styles(_ text: String) -> [MarkdownStyler.Style] {
        runs(text).map(\.style)
    }

    @Test("ATX headings style the whole line and mark the #s")
    func headings() {
        for level in 1 ... 6 {
            let marker = String(repeating: "#", count: level)
            let text = "\(marker) Title"
            #expect(runs(text) == [
                MarkdownStyler.Run(
                    range: NSRange(location: 0, length: text.utf16.count),
                    style: .heading(level: level)
                ),
                MarkdownStyler.Run(
                    range: NSRange(location: 0, length: level),
                    style: .marker
                ),
            ])
        }
    }

    @Test("a heading needs a space after at most six #s")
    func headingEdges() {
        #expect(runs("#nope").isEmpty)
        #expect(runs("####### too deep").isEmpty)
        // Up to three spaces of indent still opens the construct.
        #expect(runs("   ## two").contains(MarkdownStyler.Run(
            range: NSRange(location: 0, length: 9),
            style: .heading(level: 2)
        )))
        // Four spaces is a code block, not a heading.
        #expect(runs("    ## two").isEmpty)
    }

    @Test("strong ** and __ style the span and mark each token")
    func strong() {
        #expect(runs("a **b** c") == [
            MarkdownStyler.Run(
                range: NSRange(location: 2, length: 5),
                style: .strong
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 2, length: 2),
                style: .marker
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 5, length: 2),
                style: .marker
            ),
        ])
        #expect(styles("a __b__ c") == [.strong, .marker, .marker])
    }

    @Test("emphasis * and _ style the span")
    func emphasis() {
        #expect(styles("a *b* c") == [.emphasis, .marker, .marker])
        #expect(styles("a _b_ c") == [.emphasis, .marker, .marker])
        #expect(runs("*b*").first == MarkdownStyler.Run(
            range: NSRange(location: 0, length: 3),
            style: .emphasis
        ))
    }

    @Test("**_x_** takes strong outside and emphasis inside")
    func nested() {
        #expect(runs("**_x_**") == [
            MarkdownStyler.Run(
                range: NSRange(location: 0, length: 7),
                style: .strong
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 0, length: 2),
                style: .marker
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 5, length: 2),
                style: .marker
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 2, length: 3),
                style: .emphasis
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 2, length: 1),
                style: .marker
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 4, length: 1),
                style: .marker
            ),
        ])
        // Emphasis wraps strong the same way.
        #expect(styles("*a **b** c*") == [
            .emphasis, .marker, .marker, .strong, .marker, .marker,
        ])
    }

    @Test("***x*** takes emphasis outside and strong inside")
    func triple() {
        #expect(runs("***word***") == [
            MarkdownStyler.Run(
                range: NSRange(location: 0, length: 10),
                style: .emphasis
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 0, length: 1),
                style: .marker
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 9, length: 1),
                style: .marker
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 1, length: 8),
                style: .strong
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 1, length: 2),
                style: .marker
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 7, length: 2),
                style: .marker
            ),
        ])
        // Strong closing first leaves the outer star to emphasis…
        #expect(runs("***a** b*").filter { $0.style != .marker } == [
            MarkdownStyler.Run(
                range: NSRange(location: 0, length: 9),
                style: .emphasis
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 1, length: 5),
                style: .strong
            ),
        ])
        // …and emphasis closing first leaves the outer pair to strong.
        #expect(runs("***a* b**").filter { $0.style != .marker } == [
            MarkdownStyler.Run(
                range: NSRange(location: 0, length: 9),
                style: .strong
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 2, length: 3),
                style: .emphasis
            ),
        ])
    }

    @Test("a closing run of three closes two spans")
    func asymmetricClosers() {
        /// The styled spans, markers left out.
        func spans(_ text: String) -> [MarkdownStyler.Run] {
            runs(text).filter { $0.style != .marker }
        }

        #expect(spans("*a **b***") == [
            MarkdownStyler.Run(range: NSRange(location: 0, length: 9), style: .emphasis),
            MarkdownStyler.Run(range: NSRange(location: 3, length: 5), style: .strong),
        ])
        #expect(spans("**a *b***") == [
            MarkdownStyler.Run(range: NSRange(location: 0, length: 9), style: .strong),
            MarkdownStyler.Run(range: NSRange(location: 4, length: 3), style: .emphasis),
        ])
        // What italicizing or bolding the end of a span produces.
        #expect(spans("**a*b***") == [
            MarkdownStyler.Run(range: NSRange(location: 0, length: 8), style: .strong),
            MarkdownStyler.Run(range: NSRange(location: 3, length: 3), style: .emphasis),
        ])
        #expect(spans("*a**b***") == [
            MarkdownStyler.Run(range: NSRange(location: 0, length: 8), style: .emphasis),
            MarkdownStyler.Run(range: NSRange(location: 2, length: 5), style: .strong),
        ])
        // CommonMark's rule of three: `**` inside a word can't close a
        // lone `*`.
        #expect(spans("*foo**bar**baz*") == [
            MarkdownStyler.Run(range: NSRange(location: 0, length: 15), style: .emphasis),
            MarkdownStyler.Run(range: NSRange(location: 4, length: 7), style: .strong),
        ])
    }

    @Test("flanking reads whole Unicode characters beside a run")
    func unicodeNeighbors() {
        /// The styled spans, markers left out.
        func spans(_ text: String) -> [MarkdownStyler.Run] {
            runs(text).filter { $0.style != .marker }
        }

        // An emoji is a symbol — punctuation to CommonMark — not a
        // letter, so `_` beside it still opens and closes.
        #expect(spans("_hello_😀") == [
            MarkdownStyler.Run(range: NSRange(location: 0, length: 7), style: .emphasis),
        ])
        #expect(spans("😀_hello_") == [
            MarkdownStyler.Run(range: NSRange(location: 2, length: 7), style: .emphasis),
        ])
        #expect(spans("*😀*") == [
            MarkdownStyler.Run(range: NSRange(location: 0, length: 4), style: .emphasis),
        ])
        // A no-break space is whitespace.
        #expect(spans("_hello_\u{00A0}next") == [
            MarkdownStyler.Run(range: NSRange(location: 0, length: 7), style: .emphasis),
        ])
        #expect(spans("*hello\u{00A0}*").isEmpty)
        // A vertical tab is no CommonMark whitespace.
        #expect(spans("*x\u{000B}*") == [
            MarkdownStyler.Run(range: NSRange(location: 0, length: 4), style: .emphasis),
        ])
    }

    @Test("unmatched markers cost linear time, not quadratic")
    func unmatchedMarkersStayFast() {
        // Every `*` can open and none can close (each follows a
        // space): a rescan per opener took seconds on this 96 KB
        // paragraph; one pass takes milliseconds.
        let paragraph = String(repeating: "*a ", count: 32_000)
        let clock = ContinuousClock()
        let elapsed = clock.measure {
            #expect(MarkdownStyler.styles(forParagraph: paragraph).isEmpty)
        }
        #expect(elapsed < .seconds(10))

        // Every `**` both opens and closes, so the rule of three bars
        // it from the `*` openers below: without CommonMark's openers
        // bottom, each one would rescan them all.
        let barred = String(repeating: "*a ", count: 16_000)
            + String(repeating: "b**c ", count: 16_000)
        let barredElapsed = clock.measure {
            #expect(MarkdownStyler.styles(forParagraph: barred).count == 8_000 * 3)
        }
        #expect(barredElapsed < .seconds(10))
    }

    @Test("unclosed markers style nothing")
    func unclosed() {
        #expect(runs("a *b c").isEmpty)
        #expect(runs("a **b c").isEmpty)
        #expect(runs("a _b c").isEmpty)
        // A stray close is literal too, and a space next to a marker
        // keeps it from opening or closing.
        #expect(runs("a b* c").isEmpty)
        #expect(runs("a *b *").isEmpty)
        #expect(runs("a * b*").isEmpty)
        // A complete pair still styles among leftovers.
        #expect(styles("*a* b*") == [.emphasis, .marker, .marker])
    }

    @Test("intraword underscores are word characters")
    func intraword() {
        #expect(runs("snake_case_word").isEmpty)
        #expect(runs("a_b_c").isEmpty)
        #expect(runs("a__b__c").isEmpty)
        #expect(runs("get_thing_done").isEmpty)
        // _wrapped_ still styles, and intraword * stays emphasis.
        #expect(styles("a _b_ c") == [.emphasis, .marker, .marker])
        #expect(styles("a*b*c") == [.emphasis, .marker, .marker])
    }

    @Test("list items at line starts mark the marker")
    func lists() {
        for marker in ["- item", "* item", "+ item"] {
            #expect(runs(marker) == [
                MarkdownStyler.Run(
                    range: NSRange(location: 0, length: marker.utf16.count),
                    style: .listItem
                ),
                MarkdownStyler.Run(
                    range: NSRange(location: 0, length: 1),
                    style: .marker
                ),
            ])
        }
        #expect(runs("1. item") == [
            MarkdownStyler.Run(
                range: NSRange(location: 0, length: 7),
                style: .listItem
            ),
            MarkdownStyler.Run(
                range: NSRange(location: 0, length: 2),
                style: .marker
            ),
        ])
        #expect(runs("23) item").first?.style == .listItem)
        // Up to three spaces of indent; four is a code block.
        #expect(runs("  - item").contains(MarkdownStyler.Run(
            range: NSRange(location: 0, length: 8),
            style: .listItem
        )))
        #expect(runs("   - item").contains(MarkdownStyler.Run(
            range: NSRange(location: 0, length: 9),
            style: .listItem
        )))
        #expect(runs("    - item").isEmpty)
        // The marker needs its space.
        #expect(runs("-item").isEmpty)
        #expect(runs("1.item").isEmpty)
        // Inline styling still works inside an item.
        #expect(styles("- *x*") == [
            .listItem, .marker, .emphasis, .marker, .marker,
        ])
    }

    @Test("ranges are UTF-16 around emoji and CJK")
    func utf16() {
        // 😀 is two UTF-16 units: `**` starts at 5, not 4.
        #expect(runs("😀 a **b**").contains(MarkdownStyler.Run(
            range: NSRange(location: 5, length: 5),
            style: .strong
        )))
        #expect(runs("日本 **x**").first?.style == .strong)
        #expect(
            runs("日本語 **x**").filter { $0.style == .marker }
                .map(\.range.location) == [4, 7]
        )
    }

    @Test("markers style around Arabic text")
    func arabic() {
        // Arabic letters are alphanumeric, so _ between them is a
        // word character…
        #expect(runs("أ_b_").isEmpty)
        // …while the ASCII markers still style around it.
        #expect(runs("**مرحبا**").first?.style == .strong)
        #expect(runs("## عربي").first?.style == .heading(level: 2))
        #expect(styles("- عربي") == [.listItem, .marker])
    }

    @Test("block constructs apply per line")
    func multiLine() {
        #expect(runs("a\n- b").contains(MarkdownStyler.Run(
            range: NSRange(location: 2, length: 3),
            style: .listItem
        )))
        #expect(runs("a\n# b").contains(MarkdownStyler.Run(
            range: NSRange(location: 2, length: 3),
            style: .heading(level: 1)
        )))
    }
}
