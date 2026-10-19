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
/// characters the sanitizer's serializer keeps (`[` `]` in a query); the
/// decision compares destinations, not spellings, and unions the warnings
/// of hrefs that name one destination.
@Test
func readerMatchesDestinationsAcrossPercentEncoding() throws {
    let sanitized = "https://example.test/search?filter[status]=open"
    let data = try JSONSerialization.data(withJSONObject: [
        "v": 1, "resources": [], "attachments": [],
        "links": [
            ["href": sanitized, "warnings": []],
            ["href": "https://Example.test/caf%C3%A9", "warnings": ["raw_ip"]],
            ["href": "https://example.test/café", "warnings": ["text_domain"]],
        ],
    ])
    let manifest = try RenditionManifest.parse(data)
    let clicked = try #require(URL(string: "https://example.test/search?filter%5Bstatus%5D=open"))
    #expect(ReaderLinkDecision.decide(clicked, manifest: manifest) == .open(clicked))
    let cafe = try #require(URL(string: "https://example.test/caf%C3%A9"))
    #expect(ReaderLinkDecision.decide(cafe, manifest: manifest) == .confirm(cafe, reasons: [
        "The destination uses an IP address instead of a domain name.",
        "The link text names a different domain.",
    ]))
    let elsewhere = try #require(URL(string: "https://example.test/search?filter%5Bstatus%5D=closed"))
    #expect(ReaderLinkDecision.decide(elsewhere, manifest: manifest) == .refuse)
}

/// Escaping a component's own separators changes the destination: a
/// listed `a%2Fb` doesn't admit `a/b`, nor an escaped `&` a new parameter.
@Test
func readerKeepsEncodedSeparatorsDistinct() throws {
    let data = try JSONSerialization.data(withJSONObject: [
        "v": 1, "resources": [], "attachments": [],
        "links": [
            ["href": "https://example.test/a%2Fb", "warnings": []],
            ["href": "https://example.test/?a=x%26admin%3Dtrue", "warnings": []],
        ],
    ])
    let manifest = try RenditionManifest.parse(data)
    for unlisted in ["https://example.test/a/b", "https://example.test/?a=x&admin=true"] {
        let url = try #require(URL(string: unlisted))
        #expect(ReaderLinkDecision.decide(url, manifest: manifest) == .refuse, "\(unlisted)")
    }
    for listed in ["https://example.test/a%2fb", "https://example.test/?a=x%26admin%3dtrue"] {
        let url = try #require(URL(string: listed))
        #expect(ReaderLinkDecision.decide(url, manifest: manifest) == .open(url), "\(listed)")
    }
}

/// Destinations compare as bytes: malformed UTF-8 escapes stay distinct,
/// and so do composed and decomposed spellings of one character.
@Test
func readerComparesDestinationBytes() throws {
    let data = try JSONSerialization.data(withJSONObject: [
        "v": 1, "resources": [], "attachments": [],
        "links": [
            ["href": "https://example.test/%FF", "warnings": []],
            ["href": "https://example.test/caf%C3%A9", "warnings": []],
            ["href": "mailto:a%FF@example.test", "warnings": []],
        ],
    ])
    let manifest = try RenditionManifest.parse(data)
    for unlisted in [
        "https://example.test/%FE",
        "https://example.test/cafe%CC%81",
        "mailto:a%FE@example.test",
    ] {
        let url = try #require(URL(string: unlisted))
        #expect(ReaderLinkDecision.decide(url, manifest: manifest) == .refuse, "\(unlisted)")
    }
    let listed = try #require(URL(string: "https://example.test/%ff"))
    #expect(ReaderLinkDecision.decide(listed, manifest: manifest) == .open(listed))
}
