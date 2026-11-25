// `RecipientFenceModel` (spec/08 "Recipient check"): a failed reread
// keeps the revision's last answer, so a passing error never lifts a
// block, while another revision's failure shows nothing.

import Foundation
import Testing

@testable import SweetbriarBridge

/// Answers every query with the scripted check, or fails without one.
private actor ScriptedFence: RecipientFenceSource {
    private var answer: RecipientFence?

    func set(_ value: RecipientFence?) { answer = value }

    func recipientFence(draftId: String, revisionId: String) async throws -> RecipientFence {
        guard let answer else { throw CoreFailure.unavailable(reason: "the archive is busy") }
        return answer
    }

    func allowRecipient(accountId: String, address: String) async throws -> Receipt {
        fatalError("the model never allows")
    }

    func removeRecipientAllowance(accountId: String, address: String) async throws -> Receipt {
        fatalError("the model never removes allowances")
    }

    func recipientAllowances() async throws -> [RecipientAllowance] {
        fatalError("the model never lists allowances")
    }
}

@MainActor
struct RecipientFenceModelTests {
    @Test func aFailedRereadKeepsTheBlockAndAnotherRevisionsFailureShowsNothing() async {
        let source = ScriptedFence()
        let model = RecipientFenceModel()
        let blocked = BlockedRecipient(address: "them@example.test", accountIds: ["home"])
        await source.set(RecipientFence(ready: true, blocked: [blocked]))
        await model.refresh(source: source, draftId: "d", revisionId: "r1")
        #expect(model.blocked == [blocked])

        await source.set(nil)
        await model.refresh(source: source, draftId: "d", revisionId: "r1")
        #expect(model.blocked == [blocked])
        #expect(model.isBlocking)
        #expect(model.revisionId == "r1")

        await model.refresh(source: source, draftId: "d", revisionId: "r2")
        #expect(model.fence == nil)
        #expect(model.revisionId == nil)
        #expect(!model.isBlocking)
    }
}
