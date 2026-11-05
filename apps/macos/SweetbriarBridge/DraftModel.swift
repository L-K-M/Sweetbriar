// `DraftModel`: one draft as the compose window drives it (spec/02,
// spec/05, spec/07; plan M1.9b2c). The DTOs live in DraftTypes.swift;
// app code never sees a generated `SweetbriarCore` type.
// `load` reads the draft, `save` compares against the displayed base
// revision and exposes a typed `Conflict`, `discard` marks it discarded,
// and `stop` ends the session without cancelling a begun command.

import Foundation
import Observation
import SweetbriarCore

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
    public private(set) var isLoading = false
    /// The last same-base refusal, until a reload or a successful save.
    public private(set) var conflict: Conflict?
    /// The last read, save or discard failure.
    public private(set) var failure: (any Error)?
    public private(set) var isDiscarded = false
    /// True once `stop` ran; completions are ignored but a begun command still commits.
    public private(set) var isStopped = false

    /// The route the displayed revision carries.
    public var route: Route {
        guard let revision else { return .noRevision }
        guard let identityId = revision.identityId,
              let accountId = revision.accountId else { return .unrouted }
        return .routed(accountId: accountId, identityId: identityId)
    }

    /// Whether a save can run now.
    public var canSave: Bool {
        content != nil && !isSaving && !isStopped && !isDiscarded
    }

    private let source: any DraftStore
    @ObservationIgnored private var loadTicket: UInt64 = 0
    @ObservationIgnored private var saveTicket: UInt64 = 0
    @ObservationIgnored private var userEditVersion: UInt64 = 0
    @ObservationIgnored private var lastSave: (key: String, content: DraftContent, baseRevisionId: String?)?

    public init(source: any DraftStore, draftId: String) {
        self.source = source
        self.draftId = draftId
    }

    /// Read the draft and show its current revision, discarding unsaved content.
    public func load() async {
        await read(mode: .replace)
    }

    /// Read again: a clean editor follows the revision; an unsaved editor
    /// keeps its content, including an edit made while the read was in flight.
    public func reload() async {
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
        guard !isStopped, !isSaving, !isDiscarded, let content else { return }
        let base = baseRevisionId
        let key = saveKey(for: content, base: base)
        let ticket = saveTicket &+ 1
        saveTicket = ticket
        let version = userEditVersion
        isSaving = true
        conflict = nil
        failure = nil
        defer { if !isStopped { isSaving = false } }
        do {
            let saved = try await source.saveDraft(
                idempotencyKey: key, draftId: draftId, baseRevisionId: base, content: content
            )
            guard !isStopped, ticket == saveTicket else { return }
            baseRevisionId = saved.revisionId
            isDirty = version == userEditVersion ? false : true
            // Refresh the displayed revision behind the receipt; an
            // unsaved editor keeps its newer content. A new attachment
            // handle the save spent stays in that content; M1.9c must
            // reconcile it with the reloaded revision's retained
            // reference before the next save. No handle is rebuilt here.
            await read(mode: .preserveIfDirty)
        } catch let error as CoreFailure {
            guard case .conflict(let detail) = error else {
                guard !isStopped, ticket == saveTicket else { return }
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
            failure = error
        }
    }

    /// Mark the draft discarded under a fresh idempotency key.
    public func discard() async {
        guard !isStopped, !isDiscarded, !isSaving else { return }
        conflict = nil
        failure = nil
        do {
            _ = try await source.discardDraft(
                idempotencyKey: UUID().uuidString, draftId: draftId
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
        do {
            let draft = try await source.draft(id: draftId)
            guard !isStopped, ticket == loadTicket else { return }
            let preserve = mode == .preserveIfDirty && isDirty
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
        if let revision = draft.revision {
            self.revision = revision
            baseRevisionId = revision.revisionId
            if !preservingContent {
                content = DraftContent(revision)
                isDirty = false
            }
        } else {
            self.revision = nil
            baseRevisionId = nil
            if !preservingContent {
                content = DraftContent.empty()
                isDirty = false
            }
        }
    }

    /// The last key while content and base are unchanged, so an ambiguous
    /// attempt replays its receipt.
    private func saveKey(for content: DraftContent, base: String?) -> String {
        if let lastSave, lastSave.content == content, lastSave.baseRevisionId == base {
            return lastSave.key
        }
        let key = UUID().uuidString
        lastSave = (key, content, base)
        return key
    }
}
