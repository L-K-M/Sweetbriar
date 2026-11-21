// The Out Box pane (spec/08 "Out Box"): the selection follows the
// attempt the user chose when a reload adds rows above it, and an
// action in flight holds the pane's retirement.

import AppKit
import Foundation
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

/// Opens once; every waiter before and after resumes.
private actor Gate {
    private var isOpen = false
    private var waiters: [CheckedContinuation<Void, Never>] = []

    func wait() async {
        if isOpen { return }
        await withCheckedContinuation { waiters.append($0) }
    }

    func open() {
        isOpen = true
        for waiter in waiters { waiter.resume() }
        waiters = []
    }
}

@MainActor
private final class Flag {
    var isSet = false
}

/// An Out Box whose contents the test sets; Cancel Send can be held at
/// a gate, and the other actions aren't used.
private actor FakeOutBoxSource: SendSource {
    private var box = OutBox(live: [], history: [])
    private var cancelGate: Gate?
    private(set) var cancels = 0

    func set(_ box: OutBox) { self.box = box }
    func gateCancels(_ gate: Gate) { cancelGate = gate }

    func outBox() async throws -> OutBox { box }

    func sendDraft(
        idempotencyKey: String, draftId: String, revisionId: String, acks: [String]
    ) async throws -> SendQueued {
        throw CoreFailure.unavailable(reason: "not part of the Out Box pane")
    }

    func cancelSend(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        cancels += 1
        await cancelGate?.wait()
        return Receipt(command: "cancel_send", resultJson: "{}", createdAtMs: 0)
    }

    func resolveUnknownSend(
        idempotencyKey: String, attemptId: String, resolution: UnknownResolution
    ) async throws -> ResolvedUnknown {
        throw CoreFailure.unavailable(reason: "not part of this test")
    }

    func confirmSendRoute(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "not part of this test")
    }

    func retryRejectedRecipients(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "not part of this test")
    }

    func crossAccountNotice(
        draftId: String, revisionId: String
    ) async throws -> CrossAccountNotice? {
        throw CoreFailure.unavailable(reason: "the Out Box never queries a notice")
    }
}

private func heldRow(_ attemptId: String) -> OutBoxRow {
    OutBoxRow(
        attemptId: attemptId, draftId: "draft-\(attemptId)", accountId: "a1",
        identityId: "i1", subject: "Subject \(attemptId)", recipients: [], state: .held,
        createdAtMs: 1_699_999_999_000, updatedAtMs: 1_700_000_000_000,
        holdUntilMs: 1_700_000_005_000, resolvedAtMs: nil, sendRequestedAtMs: nil,
        protonDraftId: nil, finalReply: nil, resolution: nil,
        sentCopy: .serverSaves, waitsForConfirmation: false,
        sentCopyAppeared: true, sentCopyWarning: false, evidence: [])
}

#if canImport(Sweetbriar)
    /// A new attempt above the selected one moves it down a row; the
    /// selection moves with it instead of landing on the newcomer.
    @MainActor
    @Test(.timeLimit(.minutes(1)))
    func outBoxSelectionFollowsItsAttemptWhenARowArrivesAbove() async throws {
        let fake = FakeOutBoxSource()
        await fake.set(OutBox(live: [heldRow("a")], history: []))
        let model = OutBoxModel(source: fake)
        let pane = OutBoxViewController(model: model)
        _ = pane.view
        await model.reload()
        pane.render()

        pane.tableView.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        #expect(pane.tableView.selectedRow == 0)

        await fake.set(OutBox(live: [heldRow("new"), heldRow("a")], history: []))
        await model.reload()
        pane.render()

        #expect(pane.tableView.selectedRow == 1, "the selection stays on attempt a")
    }

    /// An archive restore waits for the pane's retirement before it
    /// swaps the archive, so an action still writing holds it.
    @MainActor
    @Test(.timeLimit(.minutes(1)))
    func outBoxRetirementWaitsForAnActionInFlight() async throws {
        let fake = FakeOutBoxSource()
        await fake.set(OutBox(live: [heldRow("a")], history: []))
        let gate = Gate()
        await fake.gateCancels(gate)
        let model = OutBoxModel(source: fake)
        let pane = OutBoxViewController(model: model)
        _ = pane.view
        await model.reload()

        pane.perform(.cancelSend, on: "a")
        var tries = 0
        while await fake.cancels == 0, tries < 500 {
            tries += 1
            try await Task.sleep(for: .milliseconds(10))
        }
        try #require(await fake.cancels == 1)

        pane.retire()
        let retired = Flag()
        let waiting = Task { @MainActor in
            await pane.waitForRetirement()
            retired.isSet = true
        }
        try await Task.sleep(for: .milliseconds(200))
        #expect(!retired.isSet, "the cancel is still writing to the old archive")

        await gate.open()
        await waiting.value
        #expect(retired.isSet)
    }
#endif
