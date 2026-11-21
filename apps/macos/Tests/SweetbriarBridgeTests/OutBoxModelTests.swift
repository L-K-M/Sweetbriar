// `OutBoxModel` against a fake source (spec/08 "Out Box"): row text
// for every state, actions per state, each action's command under a
// fresh key, one action at a time, coalesced reloads, and retirement
// dropping late results.

import Foundation
import Testing

@testable import SweetbriarBridge

/// A scripted `SendSource`: the Out Box as set, per-method failures,
/// every call recorded by name, and gates on a read or an action.
actor FakeOutBoxSource: SendSource {
    struct Call: Equatable, Sendable {
        var method: String
        var key: String
        var attemptId: String
        var resolution: UnknownResolution?
    }

    private var box = OutBox(live: [], history: [])
    private var outBoxError: (any Error)?
    private var actionError: (any Error)?
    private var readGate: Gate?
    private var actionGate: Gate?
    private(set) var calls: [Call] = []
    private(set) var reads = 0

    func set(_ box: OutBox) { self.box = box }
    func failOutBox(_ error: (any Error)?) { outBoxError = error }
    func failActions(_ error: (any Error)?) { actionError = error }
    func gateNextRead(_ gate: Gate) { readGate = gate }
    func gateNextAction(_ gate: Gate) { actionGate = gate }

    func outBox() async throws -> OutBox {
        reads += 1
        if let readGate {
            self.readGate = nil
            await readGate.wait()
        }
        if let outBoxError { throw outBoxError }
        return box
    }

    func sendDraft(
        idempotencyKey: String, draftId: String, revisionId: String, acks: [String]
    ) async throws -> SendQueued {
        fatalError("the Out Box never queues a draft")
    }

    func cancelSend(idempotencyKey key: String, attemptId: String) async throws -> Receipt {
        try await record("cancel_send", key: key, attemptId: attemptId)
        return Self.receipt("cancel_send")
    }

    func resolveUnknownSend(
        idempotencyKey key: String, attemptId: String, resolution: UnknownResolution
    ) async throws -> ResolvedUnknown {
        try await record(
            "resolve_unknown_send", key: key, attemptId: attemptId, resolution: resolution)
        return ResolvedUnknown(
            receipt: Self.receipt("resolve_unknown_send"),
            resolution: resolution,
            newAttemptId: resolution == .sendAgain ? "attempt-new" : nil)
    }

    func confirmSendRoute(idempotencyKey key: String, attemptId: String) async throws -> Receipt {
        try await record("confirm_send_route", key: key, attemptId: attemptId)
        return Self.receipt("confirm_send_route")
    }

    func retryRejectedRecipients(
        idempotencyKey key: String, attemptId: String
    ) async throws -> Receipt {
        try await record("retry_rejected_recipients", key: key, attemptId: attemptId)
        return Self.receipt("retry_rejected_recipients")
    }

    func crossAccountNotice(
        draftId: String, revisionId: String
    ) async throws -> CrossAccountNotice? {
        fatalError("the Out Box never queries a cross-account notice")
    }

    private func record(
        _ method: String, key: String, attemptId: String,
        resolution: UnknownResolution? = nil
    ) async throws {
        calls.append(
            Call(method: method, key: key, attemptId: attemptId, resolution: resolution))
        if let actionGate {
            self.actionGate = nil
            await actionGate.wait()
        }
        if let actionError { throw actionError }
    }

    private static func receipt(_ command: String) -> Receipt {
        Receipt(command: command, resultJson: "{}", createdAtMs: 0)
    }
}

private struct ScriptedFailure: Error, LocalizedError {
    var errorDescription: String? { "scripted failure" }
}

private func outBoxRow(
    _ attemptId: String,
    state: AttemptState,
    subject: String = "Hello",
    recipients: [OutBoxRecipient] = [],
    holdUntilMs: Int64? = nil,
    resolution: UnknownResolution? = nil,
    waitsForConfirmation: Bool = false,
    updatedAtMs: Int64 = 1_700_000_000_000
) -> OutBoxRow {
    OutBoxRow(
        attemptId: attemptId, draftId: "draft-\(attemptId)", accountId: "a1",
        identityId: "i1", subject: subject, recipients: recipients, state: state,
        createdAtMs: 1_699_999_999_000, updatedAtMs: updatedAtMs,
        holdUntilMs: holdUntilMs, resolvedAtMs: nil, sendRequestedAtMs: nil,
        protonDraftId: nil, finalReply: nil, resolution: resolution,
        sentCopy: .serverSaves, waitsForConfirmation: waitsForConfirmation,
        sentCopyAppeared: true, sentCopyWarning: false, evidence: [])
}

private func recipient(
    _ address: String, kind: RecipientKind = .to, state: RecipientState = .pending
) -> OutBoxRecipient {
    OutBoxRecipient(address: address, kind: kind, state: state)
}

/// Rows show the frozen subject or "No Subject", To then Cc addresses
/// only, the last-update time, and the spec's state text — the held
/// countdown against the injected clock, never below zero.
@MainActor
@Test(.timeLimit(.minutes(1)))
func outBoxRowsShowEveryStateAndNeverBcc() async throws {
    let now = Date(timeIntervalSince1970: 1_700_000_000)
    let fake = FakeOutBoxSource()
    await fake.set(
        OutBox(
            live: [
                outBoxRow(
                    "held-soon", state: .held, subject: "  Plans  ",
                    recipients: [
                        recipient("ann@example.com", kind: .to),
                        recipient("bob@example.com", kind: .cc),
                        recipient("hidden@example.com", kind: .bcc),
                    ],
                    holdUntilMs: 1_700_000_002_400,
                    updatedAtMs: 1_700_000_005_500),
                outBoxRow("held-late", state: .held, holdUntilMs: 1_699_999_999_000),
                outBoxRow(
                    "queued-wait", state: .queued, subject: "   ",
                    waitsForConfirmation: true),
                outBoxRow("queued", state: .queued),
                outBoxRow("submitting", state: .submitting),
                outBoxRow("unknown", state: .outcomeUnknown),
                outBoxRow("accepted", state: .accepted),
                outBoxRow("failed", state: .failed),
                outBoxRow("cancelled", state: .cancelled),
                outBoxRow("marked", state: .outcomeUnknown, resolution: .markedSent),
                outBoxRow("again", state: .outcomeUnknown, resolution: .sendAgain),
                outBoxRow("kept", state: .outcomeUnknown, resolution: .keptAsDraft),
            ],
            history: [outBoxRow("past", state: .accepted)]
        )
    )
    let model = OutBoxModel(source: fake, now: { now })
    await model.reload()

    #expect(model.rows.map(\.status) == [
        "Sending in 3 s",
        "Sending in 0 s",
        "Account changed; confirm sending with the new settings",
        "Queued",
        "Sending",
        "Outcome unknown",
        "Sent",
        "Failed",
        "Cancelled",
        "Marked as sent",
        "Sent again",
        "Kept as draft",
        "Sent",
    ])
    let soon = try #require(model.rows.first)
    #expect(soon.id == "held-soon")
    #expect(soon.draftId == "draft-held-soon")
    #expect(soon.subject == "Plans")
    #expect(soon.recipients == "ann@example.com, bob@example.com")
    #expect(soon.updated == Date(timeIntervalSince1970: 1_700_000_005.5))
    #expect(!soon.isHistory)
    #expect(model.rows[2].subject == OutBoxModel.noSubject)
    #expect(model.rows[2].recipients.isEmpty)
    #expect(model.rows.last?.id == "past")
    #expect(model.rows.last?.isHistory == true)
    #expect(model.rows.last?.row.attemptId == "past")
    #expect(model.hasLiveRows)
}

/// Each live row offers exactly the actions its state admits; history
/// and a retired model offer none; only Send Again and Keep as Draft
/// ask first.
@MainActor
@Test(.timeLimit(.minutes(1)))
func outBoxActionsFollowTheStateAndHistoryOffersNone() async throws {
    let rejected = recipient("bad@example.com", state: .rejectedPermanent)
    let fake = FakeOutBoxSource()
    await fake.set(
        OutBox(
            live: [
                outBoxRow("held", state: .held, holdUntilMs: 1),
                outBoxRow("queued-wait", state: .queued, waitsForConfirmation: true),
                outBoxRow("queued", state: .queued),
                outBoxRow("unknown", state: .outcomeUnknown),
                outBoxRow("resolved", state: .outcomeUnknown, resolution: .markedSent),
                outBoxRow("partial", state: .failed, recipients: [rejected]),
            ],
            history: [outBoxRow("past-rejected", state: .failed, recipients: [rejected])]
        )
    )
    let model = OutBoxModel(source: fake)
    await model.reload()

    #expect(model.actions(for: "held") == [.cancelSend])
    #expect(model.actions(for: "queued-wait") == [.cancelSend, .confirmAndSend])
    #expect(model.actions(for: "queued") == [.cancelSend])
    #expect(model.actions(for: "unknown") == [.markAsSent, .sendAgain, .keepAsDraft])
    #expect(model.actions(for: "resolved").isEmpty, "a settled unknown can't be changed")
    #expect(model.actions(for: "partial") == [.retryRejectedRecipients])
    #expect(model.actions(for: "past-rejected").isEmpty, "history offers nothing")
    #expect(model.actions(for: "missing").isEmpty)

    #expect(!OutBoxModel.needsConfirmation(.cancelSend))
    #expect(!OutBoxModel.needsConfirmation(.confirmAndSend))
    #expect(!OutBoxModel.needsConfirmation(.markAsSent))
    #expect(OutBoxModel.needsConfirmation(.sendAgain))
    #expect(OutBoxModel.needsConfirmation(.keepAsDraft))
    #expect(!OutBoxModel.needsConfirmation(.retryRejectedRecipients))

    model.retire()
    #expect(model.actions(for: "held").isEmpty)
}

/// Each action runs its own command once, under a fresh non-empty key,
/// and reloads afterwards.
@MainActor
@Test(.timeLimit(.minutes(1)))
func eachActionRunsItsCommandUnderAFreshKeyThenReloads() async throws {
    let rejected = recipient("bad@example.com", state: .rejectedTransient)
    let fake = FakeOutBoxSource()
    await fake.set(
        OutBox(
            live: [
                outBoxRow("held", state: .held, holdUntilMs: 1),
                outBoxRow("queued-wait", state: .queued, waitsForConfirmation: true),
                outBoxRow("unknown", state: .outcomeUnknown),
                outBoxRow("partial", state: .failed, recipients: [rejected]),
            ],
            history: []
        )
    )
    let model = OutBoxModel(source: fake)
    await model.reload()

    await model.perform(.cancelSend, on: "held")
    await model.perform(.confirmAndSend, on: "queued-wait")
    await model.perform(.markAsSent, on: "unknown")
    await model.perform(.sendAgain, on: "unknown")
    await model.perform(.keepAsDraft, on: "unknown")
    await model.perform(.retryRejectedRecipients, on: "partial")

    let calls = await fake.calls
    #expect(calls.map(\.method) == [
        "cancel_send",
        "confirm_send_route",
        "resolve_unknown_send",
        "resolve_unknown_send",
        "resolve_unknown_send",
        "retry_rejected_recipients",
    ])
    #expect(calls.map(\.attemptId) == [
        "held", "queued-wait", "unknown", "unknown", "unknown", "partial",
    ])
    #expect(calls.compactMap(\.resolution) == [.markedSent, .sendAgain, .keptAsDraft])
    #expect(calls.allSatisfy { !$0.key.isEmpty })
    #expect(Set(calls.map(\.key)).count == calls.count, "every action mints its own key")
    #expect(await fake.reads == 7, "the initial reload plus one after each action")
    #expect(!model.isActing)
}

/// A failed action shows the core's message and still reloads; a
/// starting action clears the old failure.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aFailedActionShowsItsMessageAndStillReloads() async throws {
    let fake = FakeOutBoxSource()
    await fake.set(
        OutBox(live: [outBoxRow("held", state: .held, holdUntilMs: 1)], history: []))
    let model = OutBoxModel(source: fake)
    await model.reload()

    await fake.failActions(CoreFailure.unavailable(reason: "offline"))
    await model.perform(.cancelSend, on: "held")
    #expect(model.actionError == "Unavailable: offline")
    #expect(await fake.reads == 2, "a failure still reloads the list")

    await fake.failActions(nil)
    await model.perform(.cancelSend, on: "held")
    #expect(model.actionError == nil)
    #expect(await fake.reads == 3)
}

/// A failure that isn't a core failure shows its localized description.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aNonCoreFailureShowsItsLocalizedDescription() async throws {
    let fake = FakeOutBoxSource()
    await fake.set(
        OutBox(live: [outBoxRow("held", state: .held, holdUntilMs: 1)], history: []))
    let model = OutBoxModel(source: fake)
    await model.reload()

    await fake.failActions(ScriptedFailure())
    await model.perform(.cancelSend, on: "held")

    #expect(model.actionError == "scripted failure")
}

/// An action the row doesn't admit — or an id that isn't listed, or a
/// retired model — calls nothing and doesn't reload.
@MainActor
@Test(.timeLimit(.minutes(1)))
func anUnadmittedActionCallsNothing() async throws {
    let fake = FakeOutBoxSource()
    await fake.set(
        OutBox(live: [outBoxRow("held", state: .held, holdUntilMs: 1)], history: []))
    let model = OutBoxModel(source: fake)
    await model.reload()

    await model.perform(.markAsSent, on: "held")
    await model.perform(.confirmAndSend, on: "held")
    await model.perform(.cancelSend, on: "missing")
    #expect(await fake.calls.isEmpty)
    #expect(await fake.reads == 1)
    #expect(model.actionError == nil)
    #expect(!model.isActing)

    model.retire()
    await model.perform(.cancelSend, on: "held")
    #expect(await fake.calls.isEmpty)
    #expect(await fake.reads == 1)
}

/// While archive writes are unavailable, an action is refused like a
/// retired model: nothing runs and the list does not reload.
@MainActor
@Test(.timeLimit(.minutes(1)))
func anActionIsRefusedWhileWritesAreUnavailable() async throws {
    let fake = FakeOutBoxSource()
    await fake.set(
        OutBox(live: [outBoxRow("held", state: .held, holdUntilMs: 1)], history: []))
    let writes = WriteFlag()
    let model = OutBoxModel(source: fake, canWrite: { writes.allowed })
    await model.reload()

    await model.perform(.cancelSend, on: "held")
    #expect(await fake.calls.isEmpty)
    #expect(await fake.reads == 1)
    #expect(model.actionError == nil)
    #expect(!model.isActing)

    writes.allowed = true
    await model.perform(.cancelSend, on: "held")
    #expect(await fake.calls.count == 1)
    #expect(await fake.reads == 2)
}

/// A second action while one runs is refused: the running call owns
/// the model until it and its reload are done.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aSecondActionWhileOneRunsIsRefused() async throws {
    let fake = FakeOutBoxSource()
    await fake.set(
        OutBox(live: [outBoxRow("held", state: .held, holdUntilMs: 1)], history: []))
    let model = OutBoxModel(source: fake)
    await model.reload()

    let gate = Gate()
    await fake.gateNextAction(gate)
    let first = Task { await model.perform(.cancelSend, on: "held") }
    try #require(await eventually { await fake.calls.count == 1 })
    #expect(model.isActing)

    await model.perform(.cancelSend, on: "held")
    #expect(await fake.calls.count == 1, "the running action owns the model")

    await gate.open()
    await first.value
    #expect(!model.isActing)
    #expect(await fake.reads == 2)
}

/// The action's reload is part of the action: until it lands, the rows
/// still show the pre-action state, so a second action is refused.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aSecondActionDuringTheActionsReloadIsRefused() async throws {
    let fake = FakeOutBoxSource()
    await fake.set(OutBox(live: [outBoxRow("u", state: .outcomeUnknown)], history: []))
    let model = OutBoxModel(source: fake)
    await model.reload()

    let gate = Gate()
    await fake.gateNextRead(gate)
    let first = Task { await model.perform(.markAsSent, on: "u") }
    try #require(await eventually { await fake.reads == 2 })
    #expect(model.isActing)

    await model.perform(.sendAgain, on: "u")
    #expect(await fake.calls.count == 1, "the action owns the model until its reload")

    await gate.open()
    await first.value
    #expect(!model.isActing)
}

/// When the action's reload joins one already running, the action
/// holds the model until that reload's extra read has landed.
@MainActor
@Test(.timeLimit(.minutes(1)))
func anActionWhoseReloadJoinsARunningOneHoldsUntilItLands() async throws {
    let fake = FakeOutBoxSource()
    await fake.set(OutBox(live: [outBoxRow("u", state: .outcomeUnknown)], history: []))
    let model = OutBoxModel(source: fake)
    await model.reload()

    let actionGate = Gate()
    await fake.gateNextAction(actionGate)
    let first = Task { await model.perform(.markAsSent, on: "u") }
    try #require(await eventually { await fake.calls.count == 1 })

    let readGate = Gate()
    await fake.gateNextRead(readGate)
    let poll = Task { await model.reload() }
    try #require(await eventually { await fake.reads == 2 })

    await actionGate.open()
    await first.value
    #expect(model.isActing, "the joined reload hasn't read the action's result yet")
    await model.perform(.sendAgain, on: "u")
    #expect(await fake.calls.count == 1)

    await readGate.open()
    await poll.value
    #expect(await fake.reads == 3)
    #expect(!model.isActing)
}

/// A reload asked for during a reload runs once more after it, however
/// many were asked for.
@MainActor
@Test(.timeLimit(.minutes(1)))
func anOutBoxReloadDuringAReloadRunsOnceMore() async throws {
    let fake = FakeOutBoxSource()
    await fake.set(OutBox(live: [outBoxRow("a", state: .held)], history: []))
    let gate = Gate()
    await fake.gateNextRead(gate)
    let model = OutBoxModel(source: fake)

    let first = Task { await model.reload() }
    try #require(await eventually { await fake.reads == 1 })
    await fake.set(
        OutBox(
            live: [outBoxRow("a", state: .held), outBoxRow("b", state: .queued)],
            history: []))
    await model.reload()
    await model.reload()

    await gate.open()
    await first.value
    #expect(await fake.reads == 2)
    #expect(model.rows.map(\.id) == ["a", "b"])
}

/// Retiring for an old archive drops the rows, a read in flight lands
/// nowhere, no row offers an action, and no reload reads again.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aRetiredOutBoxDropsLateResultsAndOffersNothing() async throws {
    let fake = FakeOutBoxSource()
    await fake.set(OutBox(live: [outBoxRow("a", state: .held)], history: []))
    let model = OutBoxModel(source: fake)
    await model.reload()

    let gate = Gate()
    await fake.gateNextRead(gate)
    await fake.set(
        OutBox(
            live: [outBoxRow("a", state: .held), outBoxRow("b", state: .queued)],
            history: []))
    let late = Task { await model.reload() }
    try #require(await eventually { await fake.reads == 2 })
    model.retire()
    await gate.open()
    await late.value

    #expect(model.rows.isEmpty)
    #expect(model.lastError == nil)
    #expect(!model.hasLiveRows)
    #expect(model.actions(for: "a").isEmpty)
    await model.reload()
    #expect(await fake.reads == 2)
}

/// A failed reload keeps the rows and says why; the next success
/// clears the error.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aFailedOutBoxReloadKeepsTheRows() async throws {
    let fake = FakeOutBoxSource()
    await fake.set(OutBox(live: [outBoxRow("a", state: .held)], history: []))
    let model = OutBoxModel(source: fake)
    await model.reload()

    await fake.failOutBox(CoreFailure.unavailable(reason: "offline"))
    await model.reload()
    #expect(model.rows.map(\.id) == ["a"])
    #expect(model.lastError as? CoreFailure == .unavailable(reason: "offline"))

    await fake.failOutBox(nil)
    await model.reload()
    #expect(model.lastError == nil)
}

/// Whether archive writes are available, flipped by a test.
@MainActor
private final class WriteFlag {
    var allowed = false
}
