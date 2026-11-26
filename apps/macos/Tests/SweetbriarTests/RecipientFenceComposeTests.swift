// The recipient check in the compose window and Settings (spec/08
// "Recipient check"; plan M6.7b): banners, the warning token, Send's
// reason, Use, Allow and its confirmation, the not-ready note, Send's
// own refusal, and the allowance list.

import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private let correspondent = "them@example.test"

private func fenceIdentity(_ account: String) -> ComposeIdentity {
    ComposeIdentity(
        identityId: "\(account)-1", accountId: account, accountName: account.capitalized, accountKind: .proton,
        email: "me@\(account).example.test", displayName: "Me", replyTo: nil, signatureSource: nil,
        isDefault: true, sendable: .validated, revision: 1)
}

/// A synthetic core for one draft with three Proton accounts. Its
/// `recipient_fence` follows spec/05's rule over the scripted sender
/// history and allowances, so Use and Allow change the answer the way
/// the core does; the rule itself is tested in sb-store and sb-engine.
private actor FenceSource: DraftStore, ComposeCatalogSource, RecipientSource, RecipientFenceSource, SendSource,
    SendSafetySettingsSource
{
    struct Allowance: Hashable {
        let accountId: String
        let address: String
    }

    private static let identities = ["work", "home", "club"].map(fenceIdentity)
    private var row = Draft(
        draftId: "fence", state: .editing, replyToOccurrenceId: nil, forwardOfMessageId: nil, revision: nil,
        attempt: nil, createdAtMs: 0, updatedAtMs: 0)
    private var saves = 0
    private var ready: Bool
    /// The sending accounts each address wrote to.
    private var senders: [String: [String]]
    private var allowed: [Allowance] = []
    private var allowanceLoadFailure: CoreFailure?
    private(set) var sends = 0
    private(set) var allowCalls: [Allowance] = []

    init(ready: Bool = true, senders: [String: [String]]) {
        self.ready = ready
        self.senders = senders
        row.revision = Self.revision(
            DraftContent(
                identityId: "work-1", format: .markdown, plainFlowed: false, subject: "Hello",
                to: [.init(address: correspondent, name: nil)], cc: [], bcc: [], bodySource: "Hi",
                attachments: []),
            seq: 0, base: nil)
    }

    func setReady(_ value: Bool) { ready = value }
    func setSenders(_ value: [String: [String]]) { senders = value }
    func setAllowanceLoadFailure(_ value: CoreFailure?) { allowanceLoadFailure = value }

    func recipientFence(draftId: String, revisionId: String) async throws -> RecipientFence {
        guard ready else { return RecipientFence(ready: false, blocked: []) }
        guard let revision = row.revision, revision.revisionId == revisionId, let account = revision.accountId else {
            throw CoreFailure.conflict(detail: "not the draft's current revision")
        }
        let blocked = (revision.to + revision.cc + revision.bcc).compactMap { recipient -> BlockedRecipient? in
            let wrote = senders[recipient.address] ?? []
            guard !wrote.isEmpty, !wrote.contains(account),
                !allowed.contains(.init(accountId: account, address: recipient.address))
            else { return nil }
            return BlockedRecipient(address: recipient.address, accountIds: wrote)
        }
        return RecipientFence(ready: true, blocked: blocked)
    }

    func allowRecipient(accountId: String, address: String) async throws -> Receipt {
        allowCalls.append(.init(accountId: accountId, address: address))
        if !allowed.contains(.init(accountId: accountId, address: address)) {
            allowed.append(.init(accountId: accountId, address: address))
        }
        return Receipt(command: "allow_recipient", resultJson: "{}", createdAtMs: 0)
    }

    func removeRecipientAllowance(accountId: String, address: String) async throws -> Receipt {
        guard let index = allowed.firstIndex(of: .init(accountId: accountId, address: address)) else {
            throw CoreFailure.notFound(what: "allowance")
        }
        allowed.remove(at: index)
        return Receipt(command: "remove_recipient_allowance", resultJson: "{}", createdAtMs: 0)
    }

    func recipientAllowances() async throws -> [RecipientAllowance] {
        if let allowanceLoadFailure { throw allowanceLoadFailure }
        return allowed.map { RecipientAllowance(accountId: $0.accountId, address: $0.address, createdAtMs: 1) }
    }

    /// Send refuses the way the engine does when the check blocks.
    func sendDraft(
        idempotencyKey: String, draftId: String, revisionId: String, acks: [String]
    ) async throws -> SendQueued {
        sends += 1
        let fence = try await recipientFence(draftId: draftId, revisionId: revisionId)
        if !fence.blocked.isEmpty {
            throw CoreFailure.conflict(
                detail: "recipient_fence: never wrote to this account, only to another: \(correspondent)")
        }
        return SendQueued(
            receipt: Receipt(command: "send_draft", resultJson: "{}", createdAtMs: 0), attemptId: "attempt-1")
    }
    func cancelSend(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        Receipt(command: "cancel_send", resultJson: "{}", createdAtMs: 0)
    }
    func resolveUnknownSend(
        idempotencyKey: String, attemptId: String, resolution: UnknownResolution
    ) async throws -> ResolvedUnknown {
        throw CoreFailure.internalError(detail: "not part of the compose send flow")
    }
    func confirmSendRoute(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        throw CoreFailure.internalError(detail: "not part of the compose send flow")
    }
    func retryRejectedRecipients(idempotencyKey: String, attemptId: String) async throws -> Receipt {
        throw CoreFailure.internalError(detail: "not part of the compose send flow")
    }
    func outBox() async throws -> OutBox {
        throw CoreFailure.internalError(detail: "not part of the compose send flow")
    }
    func crossAccountNotice(draftId: String, revisionId: String) async throws -> CrossAccountNotice? { nil }

    func settings() async throws -> [Setting] {
        [Setting(key: SendSafety.undoSecondsKey, value: "7", isDefault: true)]
    }
    func setSetting(key: String, value: String) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "the compose window never writes settings")
    }

    func draft(id: String) async throws -> Draft { row }
    func composeCatalog(preferredAccountId: String?) async throws -> ComposeCatalog {
        ComposeCatalog(identities: Self.identities, defaultIdentityId: "work-1")
    }
    func parseDraftRecipients(text: String) async throws -> [DraftRecipient] {
        text.isEmpty ? [] : [.init(address: text, name: nil)]
    }
    func recipientCompletions(prefix: String, limit: UInt32) async throws -> [DraftRecipient] { [] }
    func saveDraft(
        idempotencyKey: String, draftId: String, baseRevisionId: String?, content: DraftContent
    ) async throws -> DraftSaved {
        guard row.revision?.revisionId == baseRevisionId else {
            throw CoreFailure.conflict(detail: "Synthetic fence fixture base changed.")
        }
        saves += 1
        row.revision = Self.revision(content, seq: Int64(saves), base: baseRevisionId)
        return DraftSaved(
            receipt: .init(command: "save_draft", resultJson: "{}", createdAtMs: Int64(saves)),
            revisionId: "r\(saves)")
    }
    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt {
        throw CoreFailure.internalError(detail: "Closing must not discard a saved draft.")
    }
    func publishDraftAttachment(file: URL) async throws -> DraftAttachmentObject {
        throw CoreFailure.internalError(detail: "attachments are not used by this fixture")
    }
    func publishDraftAttachment(bytes: Data) async throws -> DraftAttachmentObject {
        throw CoreFailure.internalError(detail: "attachments are not used by this fixture")
    }
    func republishDraftAttachment(
        draftId: String, revisionId: String, reference: DraftAttachmentRef
    ) async throws -> DraftAttachmentObject {
        throw CoreFailure.internalError(detail: "attachments are not used by this fixture")
    }

    private static func revision(_ c: DraftContent, seq: Int64, base: String?) -> DraftRevision {
        DraftRevision(
            revisionId: "r\(seq)", seq: seq, baseRevisionId: base,
            accountId: identities.first { $0.identityId == c.identityId }?.accountId,
            identityId: c.identityId, format: c.format, plainFlowed: c.plainFlowed, subject: c.subject,
            to: c.to, cc: c.cc, bcc: c.bcc, bodySource: c.bodySource, attachments: [], createdAtMs: seq)
    }
}

@MainActor
private func fenceEventually(within limit: Duration = .seconds(10), _ check: @MainActor () async -> Bool) async
    -> Bool
{
    let deadline = ContinuousClock.now + limit
    while !(await check()) {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

@MainActor
private func withFenceWindow(
    _ source: FenceSource,
    body: @MainActor (ComposeWindowController) async throws -> Void
) async throws {
    let controller = ComposeWindowController(
        source: source, draftId: "fence", sendSource: source, settingsSource: source, fenceSource: source)
    controller.displayOptions = { (increaseContrast: false, reduceTransparency: false) }
    controller.showWindow(nil)
    let result: Result<Void, any Error>
    do {
        let settled = await fenceEventually {
            controller.model.content != nil && controller.recipientFence.revisionId == "r0"
        }
        try #require(settled)
        try await body(controller)
        result = .success(())
    } catch { result = .failure(error) }
    #expect(await controller.closeWithoutSavingAndWait())
    try result.get()
}

@MainActor
private func banners(_ controller: ComposeWindowController) -> [RecipientFenceBanner] {
    controller.recipientFenceBanners.isHidden
        ? [] : controller.recipientFenceBanners.arrangedSubviews.compactMap { $0 as? RecipientFenceBanner }
}

/// The strings the field's tokens show, as AppKit drew them.
@MainActor
private func shownTokens(_ field: RecipientField) -> [String] {
    let text = field.control.attributedStringValue
    var shown: [String] = []
    text.enumerateAttribute(.attachment, in: NSRange(location: 0, length: text.length)) { value, _, _ in
        if let cell = (value as? NSTextAttachment)?.attachmentCell as? NSCell { shown.append(cell.stringValue) }
    }
    return shown
}

/// Allowance lists in order; the first load waits until released.
private actor HeldAllowances: RecipientFenceSource {
    private var lists: [[RecipientAllowance]]
    private var holdNext = true
    private var held: CheckedContinuation<Void, Never>?

    init(_ lists: [[RecipientAllowance]]) { self.lists = lists }

    var isHolding: Bool { held != nil }

    func release() {
        held?.resume()
        held = nil
    }

    func recipientAllowances() async throws -> [RecipientAllowance] {
        let list = lists.removeFirst()
        if holdNext {
            holdNext = false
            await withCheckedContinuation { held = $0 }
        }
        return list
    }

    func recipientFence(draftId: String, revisionId: String) async throws -> RecipientFence {
        fatalError("Settings never reads the check")
    }
    func allowRecipient(accountId: String, address: String) async throws -> Receipt {
        fatalError("Settings never allows")
    }
    func removeRecipientAllowance(accountId: String, address: String) async throws -> Receipt {
        fatalError("this test only loads")
    }
}

@MainActor
@Suite(.serialized)
struct RecipientFenceComposeTests {
    @Test func theWordsListAccountsAndMatchTheCoresCanonicalForm() {
        #expect(RecipientFencePresentation.list(["Home"]) == "Home")
        #expect(RecipientFencePresentation.list(["Home", "Club"]) == "Home and Club")
        #expect(RecipientFencePresentation.list(["A", "B", "C"]) == "A, B and C")
        #expect(
            RecipientFencePresentation.banner(address: correspondent, accountNames: ["Home"], sendingName: "Work")
                == "them@example.test has written to Home but never to Work. Sweetbriar won't send to it from Work.")
        // The local part stays as written; the domain is lowercased.
        #expect(RecipientFencePresentation.canonical("Them@Example.TEST") == "Them@example.test")
    }

    @Test func aBlockedRecipientShowsItsBannerAndTokenAndDisablesSendWithTheReason() async throws {
        let source = FenceSource(senders: [correspondent: ["home"]])
        try await withFenceWindow(source) { controller in
            let shown = await fenceEventually { banners(controller).count == 1 }
            try #require(shown)
            let banner = try #require(banners(controller).first)
            #expect(
                banner.label.stringValue
                    == "them@example.test has written to Home but never to Work. Sweetbriar won't send to it from Work."
            )
            #expect(banner.chips.arrangedSubviews.count == 1)
            #expect(banner.useButton.title == "Use Home")
            #expect(banner.allowButton.title == "Allow for Work…")
            #expect(controller.toRecipients.displayString(for: correspondent)?.hasPrefix("\u{26A0}\u{FE0E} ") == true)
            #expect(controller.toRecipients.displayString(for: "other@example.test") == "other@example.test")
            #expect(!controller.sendButton.isEnabled)
            let reason = "Sweetbriar won't send to them@example.test from Work."
            #expect(controller.sendButton.toolTip == reason)
            #expect(controller.sendButton.accessibilityHelp() == reason)
            #expect(controller.recipientCheckNote.isHidden)
            // The menu and keyboard reach Send's action past the disabled
            // button; it stays refused.
            let action = try #require(controller.sendButton.action)
            #expect(NSApp.sendAction(action, to: controller.sendButton.target, from: controller.sendButton))
            let sent = await fenceEventually(within: .milliseconds(250)) { await source.sends > 0 }
            #expect(!sent)
        }
    }

    @Test func aTokenBlockedDuringAnEditShowsItsWarningOnceTheEditEnds() async throws {
        let source = FenceSource(senders: [:])
        try await withFenceWindow(source) { controller in
            let field = controller.toRecipients
            try #require(shownTokens(field) == [correspondent])
            let window = try #require(controller.window)
            field.control.selectText(nil)
            try #require(field.control.currentEditor() != nil)

            // History arrives while the field is being edited.
            await source.setSenders([correspondent: ["home"]])
            NotificationCenter.default.post(name: .sweetbriarMailArrived, object: nil)
            let shown = await fenceEventually { banners(controller).count == 1 }
            try #require(shown)

            try #require(window.makeFirstResponder(controller.textView))
            let warned = await fenceEventually(within: .seconds(2)) {
                shownTokens(field) == ["\u{26A0}\u{FE0E} " + correspondent]
            }
            #expect(warned)
        }
    }

    @Test func aCheckThatIsNotReadyShowsItsNoteBlocksNothingAndMailArrivalRereadsIt() async throws {
        let source = FenceSource(ready: false, senders: [correspondent: ["home"]])
        try await withFenceWindow(source) { controller in
            // The sending identity loads independently of the fence.
            let rendered = await fenceEventually {
                !controller.recipientCheckNote.isHidden && controller.sendButton.isEnabled
            }
            try #require(rendered)
            #expect(controller.recipientCheckNote.stringValue == RecipientFencePresentation.notReady)
            #expect(banners(controller).isEmpty)
            #expect(controller.toRecipients.displayString(for: correspondent) == correspondent)

            await source.setReady(true)
            NotificationCenter.default.post(name: .sweetbriarMailArrived, object: nil)
            let blocked = await fenceEventually { banners(controller).count == 1 }
            try #require(blocked)
            #expect(controller.recipientCheckNote.isHidden)
            #expect(!controller.sendButton.isEnabled)
        }
    }

    @Test func useSwitchesFromAndClearsTheBanner() async throws {
        let source = FenceSource(senders: [correspondent: ["home"]])
        try await withFenceWindow(source) { controller in
            let shown = await fenceEventually { banners(controller).count == 1 }
            try #require(shown)
            try #require(banners(controller).first).useButton.performClick(nil)
            let cleared = await fenceEventually {
                controller.identityControls.model.selectedIdentity?.identityId == "home-1"
                    && controller.recipientFence.revisionId != "r0" && banners(controller).isEmpty
            }
            try #require(cleared)
            #expect(controller.sendButton.isEnabled)
            #expect(controller.toRecipients.displayString(for: correspondent) == correspondent)
        }
    }

    @Test func anAddressThatWroteToTwoAccountsShowsBothChipsAndOffersBoth() async throws {
        let source = FenceSource(senders: [correspondent: ["home", "club"]])
        try await withFenceWindow(source) { controller in
            let shown = await fenceEventually { banners(controller).count == 1 }
            try #require(shown)
            let banner = try #require(banners(controller).first)
            #expect(banner.label.stringValue.hasPrefix("them@example.test has written to Home and Club but never"))
            #expect(banner.chips.arrangedSubviews.count == 2)
            #expect(banner.useButton.superview == nil)
            let items = banner.useMenu.itemArray.dropFirst()
            #expect(items.map(\.title) == ["Use Home", "Use Club"])
            let club = try #require(items.last)
            banner.useChosen(club)
            let cleared = await fenceEventually {
                controller.identityControls.model.selectedIdentity?.identityId == "club-1"
                    && controller.recipientFence.revisionId != "r0" && banners(controller).isEmpty
            }
            #expect(cleared)
        }
    }

    @Test func allowAsksFirstAndItsAllowanceIsListedAndRemovableInSettings() async throws {
        let source = FenceSource(senders: [correspondent: ["home"]])
        try await withFenceWindow(source) { controller in
            let shown = await fenceEventually { banners(controller).count == 1 }
            try #require(shown)

            // Cancel changes nothing.
            try #require(banners(controller).first).allowButton.performClick(nil)
            let asked = await fenceEventually { controller.allowAlert != nil }
            try #require(asked)
            let alert = try #require(controller.allowAlert)
            #expect(alert.messageText == "Allow sending to them@example.test from Work?")
            #expect(alert.buttons.map(\.title) == ["Allow", "Cancel"])
            controller.window?.endSheet(alert.window, returnCode: .alertSecondButtonReturn)
            let dismissed = await fenceEventually { controller.allowAlert == nil }
            try #require(dismissed)
            let allowed = await fenceEventually(within: .milliseconds(250)) { !(await source.allowCalls.isEmpty) }
            #expect(!allowed)
            #expect(banners(controller).count == 1)

            // Allow writes the allowance for the sending account.
            try #require(banners(controller).first).allowButton.performClick(nil)
            let again = await fenceEventually { controller.allowAlert != nil }
            try #require(again)
            controller.window?.endSheet(try #require(controller.allowAlert).window, returnCode: .alertFirstButtonReturn)
            let cleared = await fenceEventually { banners(controller).isEmpty }
            try #require(cleared)
            #expect(await source.allowCalls == [.init(accountId: "work", address: correspondent)])
            #expect(controller.sendButton.isEnabled)

            // Settings lists it by account; Remove brings the block back.
            let settings = RecipientAllowancesModel(source: source) { ["work": "Work"] }
            await settings.load()
            #expect(
                settings.groups == [
                    .init(
                        accountId: "work", accountName: "Work",
                        allowances: [.init(accountId: "work", address: correspondent, createdAtMs: 1)])
                ])
            let allowance = try #require(settings.groups.first?.allowances.first)
            await settings.remove(allowance)
            #expect(settings.groups.isEmpty)
            #expect(settings.error == nil)
            await controller.refreshRecipientFence(sameRevision: true)
            let blockedAgain = await fenceEventually { banners(controller).count == 1 }
            #expect(blockedAgain)
            #expect(!controller.sendButton.isEnabled)
        }
    }

    @Test func aSupersededAllowanceLoadNeverBringsBackARemovedRow() async throws {
        let removed = RecipientAllowance(accountId: "work", address: correspondent, createdAtMs: 1)
        let source = HeldAllowances([[removed], []])
        let settings = RecipientAllowancesModel(source: source) { [:] }
        let first = Task { await settings.load() }
        let holding = await fenceEventually { await source.isHolding }
        try #require(holding)
        await settings.load()
        #expect(settings.groups.isEmpty)

        await source.release()
        await first.value
        #expect(settings.groups.isEmpty)
        #expect(!settings.isLoading)
    }

    @Test func aFailedAllowanceLoadDoesNotShowTheEmptyMessage() async {
        let source = FenceSource(senders: [:])
        let settings = RecipientAllowancesModel(source: source) { [:] }
        #expect(!settings.showsEmptyState)

        await source.setAllowanceLoadFailure(.unavailable(reason: "Synthetic failure"))
        await settings.load()
        #expect(!settings.showsEmptyState)
        #expect(settings.error?.hasPrefix("Couldn't load the allowed addresses:") == true)

        await source.setAllowanceLoadFailure(nil)
        await settings.load()
        #expect(settings.showsEmptyState)
        #expect(settings.error == nil)
    }

    @Test func removingAMissingAllowanceShowsTheErrorAndReloads() async throws {
        let source = FenceSource(senders: [:])
        let settings = RecipientAllowancesModel(source: source) { [:] }
        await settings.remove(.init(accountId: "work", address: correspondent, createdAtMs: 1))
        #expect(settings.error?.hasPrefix("Couldn't remove them@example.test:") == true)
        #expect(settings.showsEmptyState)
        await source.setAllowanceLoadFailure(.unavailable(reason: "Synthetic failure"))
        await settings.remove(.init(accountId: "work", address: correspondent, createdAtMs: 1))
        #expect(!settings.showsEmptyState)
    }

    @Test func aRecipientFenceConflictFromSendShowsTheBanner() async throws {
        let source = FenceSource(senders: [:])
        try await withFenceWindow(source) { controller in
            #expect(banners(controller).isEmpty)
            let enabled = await fenceEventually { controller.sendButton.isEnabled }
            try #require(enabled)
            // History arrives after the window's last read: only Send's
            // refusal tells the window.
            await source.setSenders([correspondent: ["home"]])
            controller.sendButton.performClick(nil)
            let shown = await fenceEventually { banners(controller).count == 1 }
            try #require(shown)
            #expect(await source.sends == 1)
            #expect(!controller.sendButton.isEnabled)
        }
    }

    /// The bridge maps the core's answers: no allowances on a fresh
    /// archive, and the local archive can't be allowed to send.
    @Test func theCoreBridgeMapsTheRecipientCommands() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("sb-fence-\(UUID())")
        defer { try? FileManager.default.removeItem(at: root) }
        let core = try await CoreBridge.open(
            config: .init(
                dataRoot: root.appendingPathComponent("data").path,
                cacheRoot: root.appendingPathComponent("cache").path),
            snapshots: FrameBatcher<[Setting]>(clock: FenceClock()) { _ in })
        let fence: any RecipientFenceSource = core
        do {
            #expect(try await fence.recipientAllowances().isEmpty)
            await #expect(throws: CoreFailure.self) {
                _ = try await fence.allowRecipient(accountId: "archive", address: correspondent)
            }
        } catch {
            await core.shutdown()
            throw error
        }
        await core.shutdown()
    }
}

private struct FenceClock: FrameClock {
    func nextFrame() async { await Task.yield() }
}
