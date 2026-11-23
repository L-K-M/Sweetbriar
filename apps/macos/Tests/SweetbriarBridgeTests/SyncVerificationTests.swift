// Human verification during sync from the Activity row (spec/08
// "Verifying during sync"): which action a row offers, the page
// Verify… opens, the token or none that goes back, and the refusal a
// row shows.

import Foundation
import Testing

@testable import SweetbriarBridge

private actor FakeVerificationSource: SyncVerificationSource {
    var challenge: SyncVerificationChallenge?
    var refusal: CoreFailure?
    private(set) var sent: [(accountId: String, token: String?, tokenType: String?)] = []

    init(challenge: SyncVerificationChallenge?) { self.challenge = challenge }

    func refuse(_ failure: CoreFailure?) { refusal = failure }

    func protonVerification(accountId: String) async throws -> SyncVerificationChallenge? {
        if let refusal { throw refusal }
        return challenge
    }

    func verifyProtonAccount(accountId: String, token: String?, tokenType: String?) async throws {
        if let refusal { throw refusal }
        sent.append((accountId, token, tokenType))
    }
}

private let page = "https://verify.proton.me/?methods=captcha&token=synthetic-challenge"

@Suite("sync verification")
@MainActor
struct SyncVerificationTests {
    @Test func aRowOffersVerifyWhileTheChallengeIsHeldThenTryAgain() {
        #expect(ActivityPresentation.verifyAction(.verify(challengeHeld: true)) == .verify)
        #expect(ActivityPresentation.verifyAction(.verify(challengeHeld: false)) == .tryAgain)
        for condition: AccountCondition in [.signInAgain, .paused, .disconnected, .healthy] {
            #expect(ActivityPresentation.verifyAction(condition) == nil)
        }
        #expect(SyncVerifyAction.verify.title == "Verify…")
        #expect(SyncVerifyAction.tryAgain.title == "Try Again")
    }

    @Test func verifyOpensProtonsPageAndSendsTheSheetsToken() async {
        let source = FakeVerificationSource(
            challenge: SyncVerificationChallenge(url: page, methods: ["captcha"]))
        let model = SyncVerificationModel(source: source)

        let url = await model.challengePage(accountId: "proton")
        #expect(url?.absoluteString == page)
        #expect(model.refusals.isEmpty)

        await model.verify(accountId: "proton", token: "answer", tokenType: "captcha")
        let sent = await source.sent
        #expect(sent.count == 1)
        #expect(sent.first?.token == "answer" && sent.first?.tokenType == "captcha")
        #expect(model.busy.isEmpty)
    }

    @Test func tryAgainSendsNoToken() async {
        let source = FakeVerificationSource(challenge: nil)
        let model = SyncVerificationModel(source: source)
        await model.verify(accountId: "proton")
        let sent = await source.sent
        #expect(sent.count == 1 && sent.first?.token == nil && sent.first?.tokenType == nil)
        #expect(model.refusals.isEmpty)
    }

    /// No challenge held, or a page off Proton's verification host:
    /// nothing opens, and the row says why without the URL.
    @Test func verifyOpensNothingWithoutAChallengeOrOnAnotherHost() async {
        let gone = SyncVerificationModel(source: FakeVerificationSource(challenge: nil))
        #expect(await gone.challengePage(accountId: "proton") == nil)
        #expect(gone.refusals["proton"] == "Proton's challenge is no longer here. Try Again.")

        let elsewhere = SyncVerificationModel(
            source: FakeVerificationSource(
                challenge: SyncVerificationChallenge(
                    url: "https://example.test/?token=synthetic-challenge", methods: ["captcha"])))
        #expect(await elsewhere.challengePage(accountId: "proton") == nil)
        let refusal = elsewhere.refusals["proton"] ?? ""
        #expect(refusal == "Sweetbriar won't open the page Proton named.")
        #expect(!refusal.contains("synthetic-challenge"))
    }

    /// A refusal shows on its row and the next attempt clears it.
    @Test func aRefusalShowsOnItsRowUntilTheNextAttempt() async {
        let source = FakeVerificationSource(challenge: nil)
        let model = SyncVerificationModel(source: source)
        await source.refuse(.conflict(detail: "not waiting for human verification"))
        await model.verify(accountId: "proton")
        #expect(model.refusals["proton"] != nil)
        #expect(model.refusals["other"] == nil)

        await source.refuse(nil)
        await model.verify(accountId: "proton")
        #expect(model.refusals["proton"] == nil)
        #expect(await source.sent.count == 1)
    }

    /// A refusal shows while the account still asks to be verified,
    /// and not once it has moved on.
    @Test func aRefusalShowsOnlyWhileTheAccountAsksToBeVerified() {
        let refusal = "Proton's challenge is no longer here. Try Again."
        for held in [true, false] {
            #expect(
                ActivityPresentation.verifyRefusal(.verify(challengeHeld: held), refusal: refusal)
                    == refusal)
        }
        #expect(ActivityPresentation.verifyRefusal(.verify(challengeHeld: true), refusal: nil) == nil)
        for condition: AccountCondition in [.signInAgain, .paused, .disconnected, .healthy] {
            #expect(ActivityPresentation.verifyRefusal(condition, refusal: refusal) == nil)
        }
    }

    /// The challenge's description never carries its URL's token.
    @Test func aChallengeDescriptionLeavesOutItsURL() {
        let challenge = SyncVerificationChallenge(url: page, methods: ["captcha"])
        #expect(!String(describing: challenge).contains("synthetic-challenge"))
    }
}
