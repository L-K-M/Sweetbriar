import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

/// Deliberately ignores cancellation until its actual answer is released.
private actor HeldRecipientCompletion: RecipientSource {
    private(set) var requests: [String] = []
    private var retired = false
    private var pending: [(String, CheckedContinuation<Result<[DraftRecipient], CoreFailure>, Never>)] = []
    func parseDraftRecipients(text: String) async throws -> [DraftRecipient] {
        throw CoreFailure.unavailable(reason: "completion-only synthetic source")
    }
    func recipientCompletions(prefix: String, limit: UInt32) async throws -> [DraftRecipient] {
        guard !retired else { throw CoreFailure.unavailable(reason: "fixture retired") }
        requests.append(prefix)
        return try await withCheckedContinuation { pending.append((prefix, $0)) }.get()
    }
    func answer(_ prefix: String, _ answer: Result<[DraftRecipient], CoreFailure>) {
        let matching = pending.filter { $0.0 == prefix }
        pending.removeAll { $0.0 == prefix }
        for (_, continuation) in matching { continuation.resume(returning: answer) }
    }
    func releaseAll() {
        retired = true
        let held = pending
        pending.removeAll()
        for (_, continuation) in held {
            continuation.resume(returning: .failure(.unavailable(reason: "fixture retired")))
        }
    }
}

@MainActor
private func completionEventually(_ predicate: @MainActor () async -> Bool) async throws {
    let deadline = ContinuousClock.now + .seconds(5)
    while !(await predicate()) {
        if ContinuousClock.now >= deadline {
            throw CoreFailure.internalError(detail: "completion checkpoint did not arrive")
        }
        try await Task.sleep(for: .milliseconds(5))
    }
}

@MainActor
private func withCompletionField(
    _ body: @MainActor (RecipientField, HeldRecipientCompletion) async throws -> Void
) async throws {
    let source = HeldRecipientCompletion()
    let field = RecipientField(label: "To", source: source)
    let result: Result<Void, any Error>
    do {
        try await body(field, source)
        result = .success(())
    } catch { result = .failure(error) }
    await source.releaseAll()
    await field.stopAndWait()
    try result.get()
}

@Suite(.serialized) @MainActor
struct RecipientCompletionTests {
    @Test func nativeRepresentedObjectsUseByteExactValueEqualityForStableLayout() async throws {
        try await withCompletionField { field, _ in
            let first = try #require(
                field.tokenField(field.control, representedObjectForEditing: "Caf\u{e9} <a@example.test>")
                    as? NSObject)
            let same = try #require(
                field.tokenField(field.control, representedObjectForEditing: "Caf\u{e9} <a@example.test>")
                    as? NSObject)
            let distinct = try #require(
                field.tokenField(field.control, representedObjectForEditing: "Cafe\u{301} <a@example.test>")
                    as? NSObject)
            #expect(first !== same && first.isEqual(same) && first.hash == same.hash)
            #expect(!first.isEqual(distinct))
        }
    }

    @Test func staleCompletionCannotReplaceNewerLiteralInputAndRepeatedRequestsCoalesce() async throws {
        try await withCompletionField { field, source in
            var publications = 0
            field.presentationDidChange = { publications += 1 }
            @MainActor func request(_ text: String) -> [String] {
                field.tokenField(
                    field.control, completionsForSubstring: text,
                    indexOfToken: 0, indexOfSelectedItem: nil) as? [String] ?? []
            }
            #expect(request("old").isEmpty)
            try await completionEventually { await source.requests == ["old"] }
            field.controlTextDidChange(Notification(name: NSControl.textDidChangeNotification))
            let literal = "new_%\\"
            #expect(request(literal).isEmpty)
            #expect(request(literal).isEmpty)
            await source.answer("old", .success([DraftRecipient(address: "stale@example.test", name: nil)]))
            try await completionEventually { await source.requests == ["old", literal] }
            #expect(publications == 0)
            await source.answer(
                literal, .success([DraftRecipient(address: "New@EXAMPLE.test", name: "Current, Name")]))
            try await completionEventually { publications == 1 }
            #expect(request(literal) == ["\"Current, Name\" <New@EXAMPLE.test>"])
            #expect(await source.requests == ["old", literal])
        }
    }

    @Test func retirementAwaitsTheCancelledQueryAndNeverStartsItsQueuedReplacement() async throws {
        try await withCompletionField { field, source in
            _ = field.tokenField(
                field.control, completionsForSubstring: "held", indexOfToken: 0, indexOfSelectedItem: nil)
            try await completionEventually { await source.requests == ["held"] }
            _ = field.tokenField(
                field.control, completionsForSubstring: "queued", indexOfToken: 0, indexOfSelectedItem: nil)
            var entered = false
            var drained = false
            let retirement = Task {
                entered = true
                await field.stopAndWait()
                drained = true
            }
            try await completionEventually { entered }
            retirement.cancel()
            #expect(!drained)
            await source.answer("held", .success([DraftRecipient(address: "unused@example.test", name: nil)]))
            await retirement.value
            #expect(drained)
            #expect(await source.requests == ["held"])
            #expect(field.problem == nil)
        }
    }

    @Test func failedCompletionIsVisibleRatherThanAnEmptySuccess() async throws {
        try await withCompletionField { field, source in
            var publications = 0
            field.presentationDidChange = { publications += 1 }
            _ = field.tokenField(
                field.control, completionsForSubstring: "failure", indexOfToken: 0, indexOfSelectedItem: nil)
            try await completionEventually { await source.requests == ["failure"] }
            await source.answer("failure", .failure(.unavailable(reason: "synthetic unavailable history")))
            try await completionEventually { publications == 1 }
            #expect(field.problem?.contains("synthetic unavailable history") == true)
        }
    }
}
