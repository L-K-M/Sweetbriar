// Human verification's web view rules (plan M2.14b, spec/10 "The
// Proton API"): the main frame stays on verify.proton.me, frames load
// only Proton's CAPTCHA, and only the page's success message carries a
// token.

import Foundation
import Testing
@testable import SweetbriarBridge

private func url(_ string: String) -> URL { URL(string: string)! }

@Test func theMainFrameStaysOnTheVerificationPage() {
    #expect(HumanVerificationPolicy.allows(url("https://verify.proton.me/?methods=captcha&token=t"), mainFrame: true))
    #expect(HumanVerificationPolicy.allows(url("https://VERIFY.proton.me/"), mainFrame: true))
    for refused in [
        "http://verify.proton.me/",
        "https://verify-api.proton.me/core/v4/captcha",
        "https://proton.me/",
        "https://verify.proton.me.example.org/",
        "https://example.org/?next=verify.proton.me",
        "https://user@verify.proton.me/",
        "https://verify.proton.me:8443/",
        "about:blank",
        "file:///etc/hosts",
    ] {
        #expect(!HumanVerificationPolicy.allows(url(refused), mainFrame: true), "\(refused)")
    }
}

@Test func framesLoadOnlyProtonsCaptchaOrEmptyDocuments() {
    #expect(HumanVerificationPolicy.allows(url("https://verify-api.proton.me/core/v4/captcha?Token=t"), mainFrame: false))
    #expect(HumanVerificationPolicy.allows(url("https://verify.proton.me/"), mainFrame: false))
    #expect(HumanVerificationPolicy.allows(url("about:blank"), mainFrame: false))
    #expect(HumanVerificationPolicy.allows(url("about:srcdoc"), mainFrame: false))
    for refused in ["https://example.org/", "http://verify-api.proton.me/", "about:blank#x", "data:text/html,x"] {
        #expect(!HumanVerificationPolicy.allows(url(refused), mainFrame: false), "\(refused)")
    }
}

@Test func onlyAVerificationPageOpens() {
    #expect(HumanVerificationPolicy.startURL("https://verify.proton.me/?methods=captcha&token=t") != nil)
    #expect(HumanVerificationPolicy.startURL("https://example.org/") == nil)
    #expect(HumanVerificationPolicy.startURL("not a url") == nil)
}

@Test func theSuccessMessageCarriesTheToken() {
    let success = #"{"type":"HUMAN_VERIFICATION_SUCCESS","payload":{"token":"token-1","type":"captcha"}}"#
    #expect(HumanVerificationMessage.parse(success) == .verified(token: "token-1", tokenType: "captcha"))
    #expect(HumanVerificationMessage.parse(#"{"type":"CLOSE"}"#) == .closed)
    #expect(HumanVerificationMessage.parse(#"{"type":"ERROR","payload":{"message":"expired"}}"#) == .failed("expired"))
}

@Test func otherMessagesAreIgnored() {
    for ignored: Any in [
        #"{"type":"RESIZE","payload":{"height":400}}"#,
        #"{"type":"LOADED"}"#,
        #"{"type":"pm_captcha","token":"inner"}"#,
        #"{"type":"HUMAN_VERIFICATION_SUCCESS","payload":{"token":"","type":"captcha"}}"#,
        #"{"type":"HUMAN_VERIFICATION_SUCCESS","payload":{"token":"t"}}"#,
        "not json",
        42,
    ] {
        #expect(HumanVerificationMessage.parse(ignored) == nil, "\(ignored)")
    }
}

@Test func aMessageNeverPrintsItsToken() {
    let message = HumanVerificationMessage.verified(token: "token-secret", tokenType: "captcha")
    #expect(!String(describing: message).contains("token-secret"))
    #expect(!String(reflecting: message).contains("token-secret"))
}

@Test func theContentRulesBlockEverythingButTheAllowedHosts() throws {
    let data = Data(HumanVerificationPolicy.contentRules.utf8)
    let rules = try #require(try JSONSerialization.jsonObject(with: data) as? [[String: [String: String]]])
    #expect(rules.first?["trigger"]?["url-filter"] == ".*" && rules.first?["action"]?["type"] == "block")
    let passes = rules.dropFirst().compactMap { rule -> String? in
        rule["action"]?["type"] == "ignore-previous-rules" ? rule["trigger"]?["url-filter"] : nil
    }
    #expect(passes.count == rules.count - 1)
    #expect(Set(passes) == [
        #"^https://verify-api\.proton\.me/"#, #"^https://verify\.proton\.me/"#, "^about:", "^data:", "^blob:",
    ])
}
