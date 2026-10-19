import Foundation
import Testing
import SweetbriarBridge

@Test(arguments: ["http://example.test/a", "https://example.test/a", "mailto:reader@example.test"])
func readerOpensOnlyListedSafeDestinations(destination: String) throws {
    let data = try JSONSerialization.data(withJSONObject: [
        "v": 1, "resources": [], "attachments": [],
        "links": [["href": destination, "warnings": []]],
    ])
    let manifest = try RenditionManifest.parse(data)
    let url = try #require(URL(string: destination))
    #expect(ReaderLinkDecision.decide(url, manifest: manifest) == .open(url))
    #expect(ReaderLinkDecision.decide(try #require(URL(string: "https://other.test")), manifest: manifest) == .refuse)
}

@Test
func readerNamesEveryWarningAndRefusesOtherSchemesEvenWhenListed() throws {
    let warnings = ["text_domain", "raw_ip", "user_info", "future_warning"]
    let destinations = ["https://user@127.0.0.1/path", "javascript:alert(1)", "data:text/plain,x", "file:///tmp/x", "relative", "sweetbriar-cid://token/r1"]
    let data = try JSONSerialization.data(withJSONObject: [
        "v": 1, "resources": [], "attachments": [],
        "links": destinations.map { ["href": $0, "warnings": warnings] },
    ])
    let manifest = try RenditionManifest.parse(data)
    let url = try #require(URL(string: destinations[0]))
    #expect(ReaderLinkDecision.decide(url, manifest: manifest) == .confirm(url, reasons: [
        "The link text names a different domain.",
        "The destination uses an IP address instead of a domain name.",
        "The destination contains user information before the host name.",
        "The destination has an unrecognized safety warning (future_warning).",
    ]))
    for destination in destinations.dropFirst() {
        #expect(ReaderLinkDecision.decide(try #require(URL(string: destination)), manifest: manifest) == .refuse)
    }
}

/// WebKit hands clicks over as Foundation URLs, which percent-encode
/// characters the sanitizer's serializer keeps (`[` `]` in a query); a
/// listed destination matches after that same conversion, and only then.
@Test
func readerMatchesDestinationsAfterFoundationEscaping() throws {
    let data = try JSONSerialization.data(withJSONObject: [
        "v": 1, "resources": [], "attachments": [],
        "links": [
            ["href": "https://example.test/search?filter[status]=open", "warnings": []],
            ["href": "https://example.test/caf%C3%A9", "warnings": ["raw_ip"]],
        ],
    ])
    let manifest = try RenditionManifest.parse(data)
    let clicked = try #require(URL(string: "https://example.test/search?filter%5Bstatus%5D=open"))
    #expect(ReaderLinkDecision.decide(clicked, manifest: manifest) == .open(clicked))
    let cafe = try #require(URL(string: "https://example.test/caf%C3%A9"))
    #expect(ReaderLinkDecision.decide(cafe, manifest: manifest) == .confirm(cafe, reasons: [
        "The destination uses an IP address instead of a domain name.",
    ]))
    let elsewhere = try #require(URL(string: "https://example.test/search?filter%5Bstatus%5D=closed"))
    #expect(ReaderLinkDecision.decide(elsewhere, manifest: manifest) == .refuse)
}

/// Any other spelling is another destination: escaped and literal
/// separators, reserved characters, malformed UTF-8 escapes and Unicode
/// equivalents all differ in the request a server sees.
@Test
func readerRefusesOtherSpellingsOfAListedDestination() throws {
    let pairs = [
        ("https://example.test/a%2Fb", "https://example.test/a/b"),
        ("https://example.test/?a=x%26admin%3Dtrue", "https://example.test/?a=x&admin=true"),
        ("https://example.test/a%3Ab", "https://example.test/a:b"),
        ("mailto:a%2Cb@example.test", "mailto:a,b@example.test"),
        ("https://example.test/%FF", "https://example.test/%FE"),
        ("https://example.test/caf%C3%A9", "https://example.test/cafe%CC%81"),
    ]
    for (listed, unlisted) in pairs {
        let data = try JSONSerialization.data(withJSONObject: [
            "v": 1, "resources": [], "attachments": [],
            "links": [["href": listed, "warnings": []]],
        ])
        let manifest = try RenditionManifest.parse(data)
        let other = try #require(URL(string: unlisted))
        #expect(ReaderLinkDecision.decide(other, manifest: manifest) == .refuse, "\(unlisted)")
        let same = try #require(URL(string: listed))
        #expect(ReaderLinkDecision.decide(same, manifest: manifest) == .open(same), "\(listed)")
    }
}
