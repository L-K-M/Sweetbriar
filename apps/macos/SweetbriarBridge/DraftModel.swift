// Revision-aware compose editor state over the owned draft bridge.
import Foundation
import Observation

// MARK: - Model

/// The draft queries and commands a compose session needs (spec/07).
/// `CoreBridge` adopts it; tests substitute a fake. Keys stay with the
/// caller: a retry of an ambiguous save replays its receipt.
public protocol DraftStore: Sendable {
    func draft(id: String) async throws -> Draft
    func saveDraft(
        idempotencyKey: String, draftId: String,
        baseRevisionId: String?, content: DraftContent
    ) async throws -> DraftSaved
    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt
    /// Stream a user-selected file into a fresh publication (spec/07).
    func publishDraftAttachment(file: URL) async throws -> DraftAttachmentObject
    /// A fresh publication of a reference stored on an exact revision (spec/07).
    func republishDraftAttachment(
        draftId: String, revisionId: String, reference: DraftAttachmentRef
    ) async throws -> DraftAttachmentObject
}

extension CoreBridge: DraftStore {}

/// One draft's compose session (spec/02, spec/05; plan M1.9b2c):
/// load the displayed revision, edit, save over its base revision,
/// reload without losing unsaved edits, discard, and stop without
/// cancelling a begun native command.
@MainActor
@Observable
public final class DraftModel {
    /// A same-base save refusal (spec/02, spec/07): the unsaved
    /// content and the base it was attempted over, so M1.9c can offer
    /// keep-both.
    public struct Conflict: Sendable, Equatable {
        public var draftId: String
        public var baseRevisionId: String?
        public var content: DraftContent
        public var detail: String

        public init(
            draftId: String, baseRevisionId: String?, content: DraftContent, detail: String
        ) {
            self.draftId = draftId
            self.baseRevisionId = baseRevisionId
            self.content = content
            self.detail = detail
        }
    }

    /// The draft's sending route (spec/02): no revision yet, a
    /// revision with no identity (a local draft), or the selected
    /// identity's account and id.
    public enum Route: Sendable, Equatable {
        case noRevision
        case unrouted
        case routed(accountId: String, identityId: String)
    }

    public let draftId: String
    /// The last loaded draft record: state, timestamps and any live
    /// attempt.
    public private(set) var draft: Draft?
    /// The last loaded revision; its content is what the editor shows
    /// when the model is clean.
    public private(set) var revision: DraftRevision?
    /// The current editable content; nil until a load succeeds.
    public private(set) var content: DraftContent?
    /// The revision id the next save compares against (spec/02).
    public private(set) var baseRevisionId: String?
    /// True when `content` differs from the displayed base.
    public private(set) var isDirty = false
    public private(set) var isSaving = false
    public private(set) var isDiscarding = false
    public private(set) var isLoading = false
    /// The last same-base refusal, until a reload or a successful save.
    public private(set) var conflict: Conflict?
    /// The last read, save or discard failure.
    public private(set) var failure: (any Error)?
    public private(set) var isDiscarded = false
    /// True once `stop` ran; completions are ignored but a begun command still commits.
    public private(set) var isStopped = false

    /// An attachment taken out of the content, kept so undo or redo can put
    /// it back. A handle a save spent comes back as a stored reference;
    /// the next save republishes it when its base no longer has it.
    public struct AttachmentRestoration: Sendable {
        public let attachment: DraftAttachment
        public let index: Int
    }

    /// The route the displayed revision carries.
    public var route: Route {
        guard let revision else { return .noRevision }
        guard let identityId = revision.identityId,
              let accountId = revision.accountId else { return .unrouted }
        return .routed(accountId: accountId, identityId: identityId)
    }

    /// Whether a save can run now.
    public var canSave: Bool {
        content != nil && !isSaving && !isDiscarding && !isStopped && !isDiscarded
    }

    /// In-module ticket for editor decisions, not a storage generation.
    /// Loads/receipts do not advance it; every explicit edit does.
    var editVersion: UInt64 { userEditVersion }

    private let source: any DraftStore
    private let discardKey = UUID().uuidString
    @ObservationIgnored private var loadTicket: UInt64 = 0
    @ObservationIgnored private var saveTicket: UInt64 = 0
    @ObservationIgnored private var userEditVersion: UInt64 = 0
    @ObservationIgnored private var lastSave: (key: String, editVersion: UInt64, baseRevisionId: String?)?
    /// A save whose outcome is unknown: it may have committed and spent its
    /// handles without the receipt arriving. Its exact intent is retried
    /// before any different save, so a lost receipt is replayed, not guessed.
    @ObservationIgnored private var unresolved: PendingSave?
    /// The stored references of the revision `baseRevisionId` names.
    @ObservationIgnored private var baseAttachments: [DraftAttachmentRef] = []
    /// Revisions known to store each reference: republication authority.
    @ObservationIgnored private var sources: [(reference: DraftAttachmentRef, revisionId: String)] = []
    /// Handles an acknowledged save spent, and the revision that spent them.
    @ObservationIgnored private var spent: [(object: DraftAttachmentObject, revisionId: String)] = []

    private struct PendingSave {
        var key: String
        var editVersion: UInt64
        var baseRevisionId: String?
        var content: DraftContent
    }

    public init(source: any DraftStore, draftId: String) {
        self.source = source
        self.draftId = draftId
    }

    /// Read the draft and show its current revision. Unsaved content is
    /// discarded, except an edit that lands while the read is in flight.
    public func load() async {
        guard !isSaving, !isDiscarding else { return }
        await read(mode: .replace)
    }

    /// Read again: a clean editor follows the revision; an unsaved editor
    /// keeps its content, including an edit made while the read was in flight.
    public func reload() async {
        guard !isSaving, !isDiscarding else { return }
        await read(mode: .preserveIfDirty)
    }

    /// Replace the editable content; every edit bumps the version a
    /// save compares against, so a load or reload never counts as one.
    public func update(_ content: DraftContent) {
        guard !isStopped, !isDiscarded, self.content != nil else { return }
        self.content = content
        isDirty = true
        userEditVersion &+= 1
    }

    /// Save `content` over the displayed base revision. A retry of the
    /// same content and base replays its receipt, so a spent attachment
    /// reservation cannot block it; an edit during the save survives.
    public func save() async {
        guard canSave, self.content != nil else { return }
        let ticket = saveTicket &+ 1
        saveTicket = ticket
        isSaving = true
        invalidateReads()
        conflict = nil
        failure = nil
        defer { if !isStopped { isSaving = false } }
        guard await resolveUnknownOutcome(ticket: ticket) else { return }
        guard let content else { return }
        let base = baseRevisionId
        let key = saveKey(base: base)
        let version = userEditVersion
        var sent = content
        var began = false
        do {
            sent = try await withAuthority(content)
            guard !isStopped, ticket == saveTicket else { return }
            began = true
            let saved = try await source.saveDraft(
                idempotencyKey: key, draftId: draftId, baseRevisionId: base, content: sent
            )
            guard !isStopped, ticket == saveTicket else { return }
            acknowledge(saved, sent: sent, shown: content)
            isDirty = version != userEditVersion
            // The receipt establishes the new base even if this read
            // fails. Later edits keep that base and the acknowledged
            // attachment references, without spending the handles again.
            await read(mode: .preserveIfDirty)
        } catch let error as CoreFailure {
            guard case .conflict(let detail) = error else {
                guard !isStopped, ticket == saveTicket else { return }
                if began { recordUnknownOutcome(key, version, base, sent) }
                failure = error
                return
            }
            guard !isStopped, ticket == saveTicket else { return }
            conflict = Conflict(
                draftId: draftId, baseRevisionId: base, content: content, detail: detail
            )
        } catch is CancellationError {
            // A cancelled-before-begin save commits nothing. A begun
            // command runs to completion and returns its receipt, so
            // this path has no result to apply.
        } catch {
            guard !isStopped, ticket == saveTicket else { return }
            if began { recordUnknownOutcome(key, version, base, sent) }
            failure = error
        }
    }

    // MARK: Attachment edits

    /// Add a published attachment at the end; nil when nothing is editable.
    @discardableResult
    public func appendAttachment(_ attachment: DraftAttachment) -> Int? {
        guard var current = editableContent else { return nil }
        current.attachments.append(attachment)
        update(current)
        return current.attachments.count - 1
    }

    /// Take out one attachment; the restoration is its undo.
    public func removeAttachment(at index: Int) -> AttachmentRestoration? {
        guard var current = editableContent, current.attachments.indices.contains(index) else {
            return nil
        }
        let removed = current.attachments.remove(at: index)
        update(current)
        return AttachmentRestoration(attachment: removed, index: index)
    }

    /// Put a removed attachment back (undo of a removal, redo of an add).
    /// A spent handle returns as its stored reference: it can't be spent again.
    @discardableResult
    public func restoreAttachment(_ restoration: AttachmentRestoration) -> Int? {
        guard var current = editableContent else { return nil }
        var attachment = restoration.attachment
        if case .new(let object, let filename, let mediaType) = attachment,
            spent.contains(where: { $0.object === object })
        {
            attachment = .retained(Self.reference(object, filename, mediaType))
        }
        let index = min(max(restoration.index, 0), current.attachments.count)
        current.attachments.insert(attachment, at: index)
        update(current)
        return index
    }

    private var editableContent: DraftContent? {
        guard !isStopped, !isDiscarded else { return nil }
        return content
    }

    /// Retry one discard intent under its original idempotency key.
    public func discard() async {
        guard !isStopped, !isDiscarded, !isSaving, !isDiscarding else { return }
        isDiscarding = true
        invalidateReads()
        defer { if !isStopped { isDiscarding = false } }
        conflict = nil
        failure = nil
        do {
            _ = try await source.discardDraft(
                idempotencyKey: discardKey, draftId: draftId
            )
            guard !isStopped else { return }
            isDiscarded = true
        } catch is CancellationError {
        } catch {
            guard !isStopped else { return }
            failure = error
        }
    }

    /// End the session: completions stop publishing, but a begun command commits.
    public func stop() {
        guard !isStopped else { return }
        isStopped = true
        loadTicket &+= 1
        saveTicket &+= 1
    }

    /// One read at a time; a newer read or `stop` supersedes this one.
    /// A reload preserves content that became dirty while it was in
    /// flight, so a completion never clears a newer edit.
    private func read(mode: ReadMode) async {
        guard !isStopped else { return }
        isLoading = true
        failure = nil
        conflict = nil
        let ticket = loadTicket &+ 1
        loadTicket = ticket
        let version = userEditVersion
        do {
            let draft = try await source.draft(id: draftId)
            guard !isStopped, ticket == loadTicket else { return }
            let preserve = userEditVersion != version || (mode == .preserveIfDirty && isDirty)
            apply(draft, preservingContent: preserve)
            failure = nil
        } catch is CancellationError {
            // A cancelled read leaves the model as it was.
        } catch {
            guard !isStopped, ticket == loadTicket else { return }
            failure = error
        }
        if ticket == loadTicket, !isStopped { isLoading = false }
    }

    /// What a read does with the editor's content.
    private enum ReadMode {
        /// Show the loaded revision, discarding unsaved content.
        case replace
        /// Follow the loaded revision unless the editor is dirty.
        case preserveIfDirty
    }

    private func apply(_ draft: Draft, preservingContent: Bool) {
        self.draft = draft
        isDiscarded = draft.state == .discarded
        if let revision = draft.revision { recordSources(revision.attachments, in: revision.revisionId) }
        if preservingContent {
            // A remote revision is not the base of these unsaved edits.
            // A read of our acknowledged base can fill in its metadata.
            if draft.revision?.revisionId == baseRevisionId { revision = draft.revision }
            return
        }
        lastSave = nil
        unresolved = nil
        if let revision = draft.revision {
            self.revision = revision
            baseRevisionId = revision.revisionId
            baseAttachments = revision.attachments
            content = DraftContent(revision)
        } else {
            baseAttachments = []
            self.revision = nil
            baseRevisionId = nil
            content = DraftContent.empty()
        }
        isDirty = false
    }

    private func invalidateReads() {
        loadTicket &+= 1
        isLoading = false
    }

    /// Retry a save whose outcome is unknown under its own key, before saving
    /// anything different. A replay answers the lost receipt; otherwise that
    /// older content commits and the current save follows it. A conflict
    /// means it never committed. False when the save must stop here.
    private func resolveUnknownOutcome(ticket: UInt64) async -> Bool {
        guard let pending = unresolved else { return true }
        if pending.editVersion == userEditVersion, pending.baseRevisionId == baseRevisionId {
            // The current save is that same intent under that same key.
            return true
        }
        do {
            let saved = try await source.saveDraft(
                idempotencyKey: pending.key, draftId: draftId,
                baseRevisionId: pending.baseRevisionId, content: pending.content)
            guard !isStopped, ticket == saveTicket else { return false }
            acknowledge(saved, sent: pending.content, shown: pending.content)
            return true
        } catch CoreFailure.conflict {
            guard !isStopped, ticket == saveTicket else { return false }
            unresolved = nil
            return true
        } catch is CancellationError {
            return false
        } catch {
            guard !isStopped, ticket == saveTicket else { return false }
            failure = error
            return false
        }
    }

    private func recordUnknownOutcome(
        _ key: String, _ version: UInt64, _ base: String?, _ sent: DraftContent
    ) {
        unresolved = PendingSave(key: key, editVersion: version, baseRevisionId: base, content: sent)
    }

    /// A receipt: the new base, its stored references and spent handles.
    /// `shown` is the content the editor held when the save began.
    private func acknowledge(_ saved: DraftSaved, sent: DraftContent, shown: DraftContent) {
        baseRevisionId = saved.revisionId
        unresolved = nil
        baseAttachments = sent.attachments.map(Self.reference)
        recordSources(baseAttachments, in: saved.revisionId)
        for case .new(let object, _, _) in sent.attachments
        where !spent.contains(where: { $0.object === object }) {
            spent.append((object, saved.revisionId))
        }
        acknowledgeAttachments(in: shown)
    }

    /// Give every attachment the authority its save needs. A stored reference
    /// the base lacks, such as one an undo restored after a save removed it,
    /// is republished from a revision that stores it. A spent handle is a
    /// stored reference too. Anything else goes to the core unchanged.
    private func withAuthority(_ content: DraftContent) async throws -> DraftContent {
        var resolved = content
        for (index, attachment) in content.attachments.enumerated() {
            let reference: DraftAttachmentRef
            switch attachment {
            case .new(let object, let filename, let mediaType):
                guard spent.contains(where: { $0.object === object }) else { continue }
                reference = Self.reference(object, filename, mediaType)
            case .retained(let stored):
                reference = stored
            }
            if baseAttachments.contains(where: { Self.same($0, reference) }) {
                resolved.attachments[index] = .retained(reference)
                continue
            }
            guard let authority = sources.first(where: { Self.same($0.reference, reference) }) else {
                continue
            }
            let object = try await source.republishDraftAttachment(
                draftId: draftId, revisionId: authority.revisionId, reference: reference)
            resolved.attachments[index] = .new(
                object: object, filename: reference.filename, mediaType: reference.mediaType)
        }
        return resolved
    }

    private func recordSources(_ references: [DraftAttachmentRef], in revisionId: String) {
        for reference in references
        where !sources.contains(where: { Self.same($0.reference, reference) }) {
            sources.append((reference, revisionId))
        }
    }

    private static func reference(_ attachment: DraftAttachment) -> DraftAttachmentRef {
        switch attachment {
        case .new(let object, let filename, let mediaType):
            return reference(object, filename, mediaType)
        case .retained(let reference):
            return reference
        }
    }

    private static func reference(
        _ object: DraftAttachmentObject, _ filename: String, _ mediaType: String
    ) -> DraftAttachmentRef {
        DraftAttachmentRef(
            objectSha256: object.objectSha256, filename: filename,
            mediaType: mediaType, size: object.byteLength)
    }

    /// The core compares stored references byte for byte, not by Swift's
    /// canonical-equivalence string equality.
    private static func same(_ a: DraftAttachmentRef, _ b: DraftAttachmentRef) -> Bool {
        a.size == b.size && a.objectSha256.utf8.elementsEqual(b.objectSha256.utf8)
            && a.filename.utf8.elementsEqual(b.filename.utf8)
            && a.mediaType.utf8.elementsEqual(b.mediaType.utf8)
    }

    /// Only an acknowledged publication becomes a retained reference.
    /// Same bytes in an independent publication are still a new handle.
    /// Changing attachment metadata after acknowledgement requires a fresh
    /// publication; retained references must exactly match the saved base.
    private func acknowledgeAttachments(in savedContent: DraftContent) {
        guard var current = content else { return }
        var acknowledgedAny = false
        current.attachments = current.attachments.map { attachment in
            guard case .new(let object, let filename, let mediaType) = attachment else {
                return attachment
            }
            let acknowledged = savedContent.attachments.contains { saved in
                guard case .new(let original, let originalName, let originalType) = saved else {
                    return false
                }
                return original === object && originalName.utf8.elementsEqual(filename.utf8)
                    && originalType.utf8.elementsEqual(mediaType.utf8)
            }
            guard acknowledged else { return attachment }
            acknowledgedAny = true
            return .retained(DraftAttachmentRef(objectSha256: object.objectSha256,
                filename: filename, mediaType: mediaType, size: object.byteLength))
        }
        guard acknowledgedAny else { return }
        content = current
    }

    /// Retry an unchanged intent. Swift value equality would conflate
    /// distinct UTF-8 strings and independent same-byte publications.
    private func saveKey(base: String?) -> String {
        if let lastSave, lastSave.editVersion == userEditVersion, lastSave.baseRevisionId == base {
            return lastSave.key
        }
        let key = UUID().uuidString
        lastSave = (key, userEditVersion, base)
        return key
    }
}
