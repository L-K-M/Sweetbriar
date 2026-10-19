import Testing
@testable import Sweetbriar

@Test func quoteDepthCountsLeadingMarkersOnly() {
    let text = "literal **bold** > middle\n> one\n>> two\n > >  > three\n >\n  unquoted >\n\t> tab\n"
    let lines = QuoteDepth.lines(text)
    #expect(lines.map(\.depth) == [0, 1, 2, 3, 1, 0, 0, 0])
    #expect(lines.map(\.text).joined(separator: "\n") == text)
}

@Test func quoteDepthHandlesMailLineEndingsAndUnicode() {
    #expect(QuoteDepth.lines("> café\r\n> > 🐦\rplain\n") == [
        .init(text: "> café", depth: 1), .init(text: "> > 🐦", depth: 2),
        .init(text: "plain", depth: 0), .init(text: "", depth: 0)
    ])
    #expect(QuoteDepth.lines("") == [.init(text: "", depth: 0)])
    #expect(QuoteDepth.lines(">>text>more").first?.depth == 2)
}
