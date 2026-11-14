import Foundation
import Testing

@testable import SweetbriarBridge

private actor IdentityGate {
    private var opened = false
    private var waiters: [CheckedContinuation<Void, Never>] = []
    func wait() async { if !opened { await withCheckedContinuation { waiters.append($0) } } }
    func open() {
        opened = true
        let pending = waiters
        waiters.removeAll()
        for waiter in pending { waiter.resume() }
    }
}

private func identity(
    _ id: String, sendable: IdentitySendable = .validated, kind: AccountKind = .imap,
    signature: String? = nil
) -> ComposeIdentity {
    ComposeIdentity(
        identityId: id, accountId: "account-\(id)", accountName: "Account \(id)", accountKind: kind,
        email: "\(id)@EXAMPLE.test", displayName: "Name \(id)", replyTo: nil, signatureSource: signature,
        isDefault: true, sendable: sendable, revision: 1)
}
private func identityCatalog(defaultId: String? = "a") -> ComposeCatalog {
    ComposeCatalog(
        identities: [
            identity("a", signature: "Signature A"),
            identity("b", sendable: .provisional, signature: "**Signature B**"),
            identity("c", sendable: .unavailable), identity("local", kind: .local),
        ], defaultIdentityId: defaultId)
}

private actor IdentityCatalogFixture: ComposeCatalogSource {
    private let answer: ComposeCatalog
    private var failure: CoreFailure?
    private var gate: IdentityGate?
    private var retired = false
    private(set) var preferred: [String?] = []
    init(_ answer: ComposeCatalog) { self.answer = answer }
    func hold(_ gate: IdentityGate) { self.gate = gate }
    func fail(_ failure: CoreFailure?) { self.failure = failure }
    func composeCatalog(preferredAccountId: String?) async throws -> ComposeCatalog {
        guard !retired else { throw CoreFailure.unavailable(reason: "fixture retired") }
        preferred.append(preferredAccountId)
        let answer = answer
        let failure = failure
        await gate?.wait()  // Deliberately ignores cancellation until released.
        if let failure { throw failure }
        return answer
    }
    func release() async {
        retired = true
        await gate?.open()
    }
}

private func identityRevision(_ content: DraftContent, sequence: Int64 = 1) -> DraftRevision {
    DraftRevision(
        revisionId: "r\(sequence)", seq: sequence, baseRevisionId: nil,
        accountId: content.identityId.map { "account-\($0)" }, identityId: content.identityId,
        format: content.format, plainFlowed: content.plainFlowed, subject: content.subject, to: content.to,
        cc: content.cc, bcc: content.bcc, bodySource: content.bodySource,
        attachments: content.attachments.compactMap {
            if case .retained(let value) = $0 { return value }
            return nil
        }, createdAtMs: sequence)
}
private actor IdentityDraftStore: DraftStore {
    private var record: Draft
    init(content: DraftContent?, state: DraftState) {
        record = Draft(
            draftId: "draft", state: state, replyToOccurrenceId: nil, forwardOfMessageId: nil,
            revision: content.map { identityRevision($0) }, attempt: nil, createdAtMs: 1, updatedAtMs: 1)
    }
    func draft(id: String) async throws -> Draft { record }
    func saveDraft(idempotencyKey: String, draftId: String, baseRevisionId: String?, content: DraftContent)
        async throws -> DraftSaved
    {
        let sequence = (record.revision?.seq ?? 0) + 1
        record.revision = identityRevision(content, sequence: sequence)
        return DraftSaved(
            receipt: Receipt(command: "save_draft", resultJson: "{}", createdAtMs: sequence),
            revisionId: "r\(sequence)")
    }
    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "discard not used by identity fixture")
    }
}

@MainActor
private func identityEventually(_ predicate: @MainActor () async -> Bool) async throws {
    let deadline = ContinuousClock.now + .seconds(5)
    while !(await predicate()) {
        guard ContinuousClock.now < deadline else {
            throw CoreFailure.internalError(detail: "identity checkpoint did not arrive")
        }
        try await Task.sleep(for: .milliseconds(5))
    }
}
@MainActor
private func withIdentityFixture(
    content: DraftContent? = nil, state: DraftState = .editing,
    catalog: ComposeCatalog = identityCatalog(), preferred: String? = nil,
    canApply: @escaping @MainActor (ComposeIdentityModel.Transition) -> Bool = { _ in true },
    body: @MainActor (DraftModel, ComposeIdentityModel, IdentityCatalogFixture) async throws -> Void
) async throws {
    let store = IdentityDraftStore(content: content, state: state)
    let draft = DraftModel(source: store, draftId: "draft")
    await draft.load()
    let source = IdentityCatalogFixture(catalog)
    let model = ComposeIdentityModel(
        draft: draft, source: source, preferredAccountId: preferred, canApply: canApply)
    let result: Result<Void, any Error>
    do {
        try await body(draft, model, source)
        result = .success(())
    } catch { result = .failure(error) }
    await source.release()
    await model.stopAndWait()
    draft.stop()
    try result.get()
}

enum IdentityReadReplacement: CaseIterable, Sendable { case body, normalization, format, route }

@Suite @MainActor
struct ComposeIdentityModelTests {
    @Test func resolvedDefaultIsNotTheFirstChoiceAndPreservesFormatNamesAndOtherFields() async throws {
        try await withIdentityFixture(
            catalog: identityCatalog(defaultId: "b"), preferred: "selected-account",
            body: { draft, model, source in
                var content = try #require(draft.content)
                content.subject = "Keep subject"
                content.to = [DraftRecipient(address: "User@EXAMPLE.test", name: "Doe, Alex")]
                content.cc = [DraftRecipient(address: "copy@example.test", name: "Copy")]
                content.bcc = [DraftRecipient(address: "hidden@example.test", name: nil)]
                content.attachments = [
                    .retained(
                        DraftAttachmentRef(
                            objectSha256: String(repeating: "a", count: 64),
                            filename: "Kept.txt", mediaType: "text/plain", size: 17))
                ]
                content.plainFlowed = false
                draft.update(content)
                var transitions = 0
                model.onTransition = { change in
                    transitions += 1
                    #expect(
                        draft.content?.identityId == change.identityId
                            && draft.content?.bodySource == change.body)
                    #expect(change.previousIdentityId == nil && change.format == .plain)
                }
                await model.reload()
                #expect(model.isReady && model.choices.map(\.identityId) == ["a", "b"])
                #expect(draft.content?.identityId == "b" && draft.content?.bodySource == "**Signature B**")
                #expect(draft.content?.format == .plain && draft.content?.plainFlowed == false)
                #expect(draft.content?.subject == content.subject && draft.content?.to == content.to)
                #expect(
                    draft.content?.cc == content.cc && draft.content?.bcc == content.bcc
                        && draft.content?.attachments == content.attachments)
                #expect(await source.preferred == ["selected-account"] && transitions == 1)
            })
    }

    @Test func missingDefaultNeverChoosesAnArbitrarySender() async throws {
        try await withIdentityFixture(catalog: identityCatalog(defaultId: nil)) { draft, model, _ in
            await model.reload()
            #expect(model.choices.count == 2 && draft.content?.identityId == nil && !draft.isDirty)
        }
    }

    @Test func savedNilAndUnavailableRoutesAreNeverReplacedByDefaults() async throws {
        var content = DraftContent.empty()
        content.bodySource = "Saved text"
        for id: String? in [nil, "c"] {
            content.identityId = id
            try await withIdentityFixture(content: content) { draft, model, _ in
                await model.reload()
                #expect(draft.content?.identityId == id && draft.content?.bodySource == "Saved text")
                #expect(model.selectedIdentity?.identityId == id)
                #expect(!model.choices.contains { $0.identityId == "c" || $0.identityId == "local" })
                #expect(!draft.isDirty)
            }
        }
    }

    @Test func explicitNilWhileLoadingBlocksTheDefault() async throws {
        try await withIdentityFixture { draft, model, source in
            let gate = IdentityGate()
            await source.hold(gate)
            let loading = Task { await model.reload() }
            try await identityEventually { await source.preferred.count == 1 }
            #expect(model.choose(identityId: nil) == .unchanged)
            await gate.open()
            await loading.value
            #expect(draft.content?.identityId == nil && draft.content?.bodySource == "")
        }
    }

    @Test(arguments: [false, true])
    func lateDefaultPreservesTypingAndAnAcknowledgedNilRoute(saved: Bool) async throws {
        try await withIdentityFixture { draft, model, source in
            let gate = IdentityGate()
            await source.hold(gate)
            let loading = Task { await model.reload() }
            try await identityEventually { await source.preferred.count == 1 }
            var content = try #require(draft.content)
            content.bodySource = "Cafe\u{301} typed while loading"
            content.format = .markdown
            draft.update(content)
            if saved { await draft.save() }
            await gate.open()
            await loading.value
            #expect(draft.content?.bodySource.utf8.elementsEqual(content.bodySource.utf8) == true)
            #expect(draft.content?.format == .markdown)
            #expect(draft.content?.identityId == (saved ? nil : "a"))
        }
    }

    @Test func onlyOwnedUneditedSignaturesAreAutomaticallyReplacedOrRemoved() async throws {
        try await withIdentityFixture { draft, model, _ in
            await model.reload()
            #expect(draft.content?.bodySource == "Signature A")
            #expect(model.choose(identityId: "b") == .applied)
            #expect(draft.content?.bodySource == "**Signature B**")
            #expect(model.choose(identityId: nil) == .applied)
            #expect(draft.content?.bodySource == "" && draft.content?.identityId == nil)
            #expect(model.choose(identityId: "a") == .applied)
            var content = try #require(draft.content)
            content.subject = "An editor change invalidates managed ownership"
            draft.update(content)
            #expect(model.choose(identityId: "b") == .needsSignatureChoice)
            let decision = try #require(model.pendingDecision)
            #expect(draft.content?.identityId == "a")
            #expect(model.resolve(decisionId: decision.id, choice: .keepText) == .applied)
            #expect(draft.content?.identityId == "b" && draft.content?.bodySource == "Signature A")
        }
    }

    @Test func unownedSavedTextIsNeverGuessedAwayAndExplicitAppendKeepsItsPrefix() async throws {
        var content = DraftContent.empty()
        content.identityId = "a"
        content.bodySource = "Authored\n\n-- \nLooks like Signature A"
        try await withIdentityFixture(content: content) { draft, model, _ in
            await model.reload()
            #expect(model.choose(identityId: "b") == .needsSignatureChoice)
            let decision = try #require(model.pendingDecision)
            #expect(model.resolve(decisionId: decision.id, choice: .append) == .applied)
            #expect(draft.content?.bodySource == content.bodySource + "\n\n**Signature B**")
            #expect(model.choose(identityId: "a") == .applied)
            #expect(draft.content?.bodySource == content.bodySource + "\n\nSignature A")
            #expect(draft.content?.format == .plain)
        }
    }

    @Test(arguments: [false, true])
    func aStaleSignatureDecisionRefusesWithoutReplacingAnyText(refresh: Bool) async throws {
        var content = DraftContent.empty()
        content.identityId = "a"
        content.bodySource = "Keep me"
        try await withIdentityFixture(content: content) { draft, model, _ in
            await model.reload()
            #expect(model.choose(identityId: "b") == .needsSignatureChoice)
            let decision = try #require(model.pendingDecision)
            if refresh {
                await model.reload()
            } else {
                var edited = content
                edited.bodySource = "Later text"
                draft.update(edited)
            }
            #expect(model.resolve(decisionId: decision.id, choice: .append) == .refused)
            #expect(draft.content?.identityId == "a")
            #expect(draft.content?.bodySource == (refresh ? "Keep me" : "Later text"))
            #expect(model.problem != nil && model.pendingDecision == nil)
        }
    }

    @Test func queryFailureKeepsAnExplicitlyStaleWholeCatalogAndRefusesNewChoices() async throws {
        var content = DraftContent.empty()
        content.identityId = "a"
        try await withIdentityFixture(content: content) { draft, model, source in
            await model.reload()
            await source.fail(.unavailable(reason: "synthetic catalog refusal"))
            await model.reload()
            guard case .failed = model.readState else {
                Issue.record("query refusal must remain visible")
                return
            }
            #expect(model.catalog?.identities.count == 4 && model.selectedIdentity?.identityId == "a")
            #expect(model.choose(identityId: "b") == .refused)
            #expect(draft.content?.identityId == "a" && !draft.isDirty)
        }
    }

    @Test func retirementAwaitsTheActualHeldAnswerDespiteACancelledWaiter() async throws {
        try await withIdentityFixture { draft, model, source in
            let gate = IdentityGate()
            await source.hold(gate)
            let loading = Task { await model.reload() }
            try await identityEventually { await source.preferred.count == 1 }
            loading.cancel()
            var entered = false
            var drained = false
            let retirement = Task {
                entered = true
                await model.stopAndWait()
                drained = true
            }
            try await identityEventually { entered }
            retirement.cancel()
            #expect(!drained && model.catalog == nil && draft.content?.identityId == nil)
            await gate.open()
            await retirement.value
            await loading.value
            #expect(drained && model.catalog == nil && !draft.isDirty)
            guard case .stopped = model.readState else {
                Issue.record("retired query must not publish")
                return
            }
        }
    }

    @Test(arguments: IdentityReadReplacement.allCases)
    func aCleanReadReplacementInvalidatesTheSignatureDecision(_ kind: IdentityReadReplacement) async throws {
        var original = DraftContent.empty(identityId: "a")
        original.bodySource = "Caf\u{e9}"
        let store = IdentityDraftStore(content: original, state: .editing)
        let draft = DraftModel(source: store, draftId: "draft")
        await draft.load()
        let source = IdentityCatalogFixture(identityCatalog(defaultId: nil))
        let model = ComposeIdentityModel(draft: draft, source: source)
        let result: Result<Void, any Error>
        do {
            await model.reload()
            #expect(model.choose(identityId: "b") == .needsSignatureChoice)
            let decision = try #require(model.pendingDecision)
            let version = draft.editVersion
            var replacement = original
            switch kind {
            case .body: replacement.bodySource = "New remote body"
            case .normalization: replacement.bodySource = "Cafe\u{301}"
            case .format: replacement.format = .markdown
            case .route: replacement.identityId = "c"
            }
            _ = try await store.saveDraft(
                idempotencyKey: "external-writer", draftId: "draft", baseRevisionId: draft.baseRevisionId,
                content: replacement)
            await draft.reload()
            #expect(draft.editVersion == version)
            #expect(model.resolve(decisionId: decision.id, choice: .append) == .refused)
            #expect(draft.content?.bodySource.utf8.elementsEqual(replacement.bodySource.utf8) == true)
            #expect(draft.content?.identityId == replacement.identityId)
            #expect(draft.content?.format == replacement.format)
            result = .success(())
        } catch { result = .failure(error) }
        await source.release()
        await model.stopAndWait()
        draft.stop()
        try result.get()
    }

    @Test func nativeAdmissionReceivesTheProposalBeforeModelMutation() async throws {
        var current: DraftModel?
        var admitted = 0
        try await withIdentityFixture(
            catalog: identityCatalog(defaultId: nil),
            canApply: { proposal in
                admitted += 1
                #expect(current?.content?.identityId == nil)
                #expect(current?.content?.bodySource == "")
                #expect(proposal.identityId == "a")
                #expect(proposal.body == "Signature A")
                return false
            },
            body: { draft, model, _ in
                current = draft
                await model.reload()
                #expect(model.choose(identityId: "a") == .refused)
                #expect(admitted == 1)
                #expect(draft.content?.identityId == nil)
                #expect(draft.content?.bodySource == "")
            })
    }

    @Test func nativeInputAdmissionRefusesBeforeAnyAtomicTransition() async throws {
        var admitted = false
        try await withIdentityFixture(
            canApply: { _ in admitted },
            body: { draft, model, _ in
                var transitions = 0
                model.onTransition = { _ in transitions += 1 }
                await model.reload()
                #expect(draft.content?.identityId == nil && transitions == 0 && model.problem != nil)
                admitted = true
                #expect(model.choose(identityId: "b") == .applied)
                #expect(draft.content?.identityId == "b" && transitions == 1)
            })
    }

    @Test func aNativeInputCommitDuringAdmissionCannotBeOverwrittenByAnOldProposal() async throws {
        var current: DraftModel?
        try await withIdentityFixture(
            catalog: identityCatalog(defaultId: nil),
            canApply: { _ in
                guard let draft = current, var content = draft.content else { return false }
                content.bodySource = "Committed native input"
                draft.update(content)
                return true
            },
            body: { draft, model, _ in
                current = draft
                await model.reload()
                #expect(model.choose(identityId: "a") == .refused)
                #expect(
                    draft.content?.identityId == nil && draft.content?.bodySource == "Committed native input")
                #expect(!model.restoreRoute(identityId: "b") && draft.content?.identityId == nil)
            })
    }

    @Test(arguments: [DraftState.queued, .sent, .discarded])
    func terminalDraftsCannotAcquireAnEditableIdentity(state: DraftState) async throws {
        try await withIdentityFixture(state: state) { draft, model, _ in
            await model.reload()
            #expect(model.choose(identityId: "a") == .refused)
            #expect(!model.restoreRoute(identityId: "a") && draft.content?.identityId == nil)
        }
    }

    @Test func cancelledChoicesAndRouteUndoKeepAllLaterContent() async throws {
        var content = DraftContent.empty()
        content.identityId = "a"
        content.bodySource = "Saved source"
        try await withIdentityFixture(content: content) { draft, model, _ in
            await model.reload()
            #expect(model.choose(identityId: "b") == .needsSignatureChoice)
            let decision = try #require(model.pendingDecision)
            model.cancelDecision()
            #expect(model.resolve(decisionId: decision.id, choice: .append) == .refused)
            var latest = try #require(draft.content)
            latest.to = [DraftRecipient(address: "later@example.test", name: "Later")]
            latest.bodySource = "Later source"
            draft.update(latest)
            #expect(model.restoreRoute(identityId: "c"))
            #expect(
                draft.content?.identityId == "c" && draft.content?.bodySource == latest.bodySource
                    && draft.content?.to == latest.to)
        }
    }

    @Test func duplicateCatalogIdentifiersRefuseTheWholeAnswer() async throws {
        let duplicate = ComposeCatalog(identities: [identity("a"), identity("a")], defaultIdentityId: "a")
        try await withIdentityFixture(catalog: duplicate) { draft, model, _ in
            await model.reload()
            guard case .failed = model.readState else {
                Issue.record("duplicate catalog must fail")
                return
            }
            #expect(model.catalog == nil && draft.content?.identityId == nil && !draft.isDirty)
        }
    }

    @Test(.timeLimit(.minutes(1)))
    func realNoSenderCatalogLoadsWithoutCreatingARouteOrRevision() async throws {
        let (root, config) = tempConfig()
        let core = try await CoreBridge.open(
            config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
        do {
            let created = try await core.createDraft(idempotencyKey: UUID().uuidString, source: .new)
            let draft = DraftModel(source: core, draftId: created.draftId)
            await draft.load()
            let model = ComposeIdentityModel(draft: draft, source: core)
            await model.reload()
            #expect(model.isReady && model.choices.isEmpty && draft.content?.identityId == nil)
            #expect(!draft.isDirty && draft.baseRevisionId == nil)
            await model.stopAndWait()
            draft.stop()
            await core.shutdown()
        } catch {
            await core.shutdown()
            throw error
        }
        try FileManager.default.removeItem(at: root)
    }
}
