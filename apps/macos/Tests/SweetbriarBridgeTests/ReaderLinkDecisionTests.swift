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
