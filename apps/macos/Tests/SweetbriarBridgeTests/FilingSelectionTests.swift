// The selection a filing command captures from a list (spec/08
// "Keyboard filing"), against the fake window source.

import Foundation
import Testing
@testable import SweetbriarBridge

@MainActor
private func openList(
    _ fake: FakeListWindowSource, spec: WindowSpec
) async throws -> ListWindowModel {
    let model = ListWindowModel(source: fake, spec: spec, clock: ManualFrameClock())
    try await model.open()
    return model
}

@MainActor
@Suite(.timeLimit(.minutes(1)))
struct FilingSelectionTests {
    /// A mailbox has one account, so one row's read answers for the
    /// whole selection, whose ids go in window order.
    @Test func aMailboxSelectionReadsOneRowForItsAccount() async throws {
        let fake = FakeListWindowSource()
        await fake.fill(1_200)
        let model = try await openList(fake, spec: .mailbox(1))

        let selection = try await model.filingSelection(of: [10, 700, 300])

        #expect(selection.parts == [
            FilingPart(accountId: "synthetic", selection: .occurrenceIds([700, 300, 10]), count: 3)
        ])
        #expect(await fake.reads.count == 1)
        await model.close()
    }

    @Test func aWholeMailboxPastTheLimitPassesItsToken() async throws {
        let fake = FakeListWindowSource()
        let generation = await fake.fill(1_200)
        let model = try await openList(fake, spec: .mailbox(1))

        let selection = try await model.filingSelection(of: Set(model.ids))

        let part = try #require(selection.parts.first)
        guard case .window(_, let tokenGeneration) = part.selection else {
            Issue.record("expected the window's token")
            return
        }
        #expect(tokenGeneration == generation)
        #expect(part.count == 1_200)
        await model.close()
    }

    @Test func anyOtherSelectionPastTheLimitIsRefused() async throws {
        let fake = FakeListWindowSource()
        await fake.fill(1_200)
        let mailbox = try await openList(fake, spec: .mailbox(1))
        await #expect(throws: FilingRefusal.tooManyRows) {
            try await mailbox.filingSelection(of: Set(mailbox.ids.prefix(1_001)))
        }
        await mailbox.close()

        let flagged = try await openList(fake, spec: .flagged)
        await #expect(throws: FilingRefusal.tooManyRows) {
            try await flagged.filingSelection(of: Set(flagged.ids))
        }
        await flagged.close()
    }

    /// Where rows mix accounts, every selected row is read, a page at a
    /// time, even far outside the cache.
    @Test func aMixedWindowReadsTheUncachedPages() async throws {
        let fake = FakeListWindowSource()
        await fake.fill(1_000)
        let model = try await openList(fake, spec: .flagged)

        let selection = try await model.filingSelection(of: [1, 2, 450])

        #expect(selection.parts.map(\.selection) == [.occurrenceIds([450, 2, 1])])
        #expect(await fake.reads.count == 2)
        await model.close()
    }

    @Test func aListThatChangedUnderTheCommandIsRefused() async throws {
        let fake = FakeListWindowSource()
        await fake.fill(1_000)
        let model = try await openList(fake, spec: .flagged)
        await fake.fill(900)

        await #expect(throws: FilingRefusal.listChanged) {
            try await model.filingSelection(of: [1])
        }
        await #expect(throws: FilingRefusal.listChanged) {
            try await model.filingSelection(of: [5_000])
        }
        await model.close()
    }
}
