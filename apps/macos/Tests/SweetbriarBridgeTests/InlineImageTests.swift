// The inline-image rules (spec/05 "inline images", spec/08 "Inline
// images"): minted Content-IDs, names and the escaped reference.

import Foundation
import Testing

@testable import SweetbriarBridge

@Suite struct InlineImageRuleTests {
    /// A lowercase UUID at `sweetbriar.invalid`, fresh each time.
    @Test func mintedContentIdsAreLowercaseUUIDsAtTheInvalidDomain() throws {
        let first = InlineImage.mintContentId()
        let pattern = try Regex(
            "^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}@sweetbriar\\.invalid$")
        #expect(first.wholeMatch(of: pattern) != nil)
        #expect(first != InlineImage.mintContentId())
    }

    @Test func theNameIsTheFileNameWithoutItsExtension() {
        #expect(InlineImage.name(forFilename: "Chart.png") == "Chart")
        #expect(InlineImage.name(forFilename: "q3.final.jpeg") == "q3.final")
        #expect(InlineImage.name(forFilename: "Image.png") == "Image")
        #expect(InlineImage.name(forFilename: "  .png") == "Image", "nothing left names it Image")
    }

    /// Nothing in a name can end the alt text, start markup or split the
    /// reference: ASCII punctuation is escaped, line breaks become spaces.
    @Test func theReferenceEscapesItsName() {
        #expect(
            InlineImage.reference(name: "Chart", contentId: "a@sweetbriar.invalid")
                == "![Chart](cid:a@sweetbriar.invalid)")
        #expect(
            InlineImage.reference(name: "Q3 [draft]*\nfinal_v2", contentId: "b@x.invalid")
                == #"![Q3 \[draft\]\* final\_v2](cid:b@x.invalid)"#)
        #expect(InlineImage.escaped("](x) `y` \\") == #"\]\(x\) \`y\` \\"#)
        #expect(InlineImage.escaped("Grüße") == "Grüße", "non-ASCII stays as is")
    }
}
