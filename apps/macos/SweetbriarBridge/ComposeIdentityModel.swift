import Foundation
import Observation

/// Sending metadata and safe local route/signature decisions. The native shell
/// supplies synchronous input admission and renders each transition atomically;
/// no sending, appearance derivation or native text editing lives here.
@MainActor @Observable
public final class ComposeIdentityModel {
    public enum ReadState {
        case unloaded, loading, loaded
        case failed(any Error)
        case stopped
    }
    public enum Change: Sendable, Equatable { case applied, unchanged, needsSignatureChoice, refused }
    public enum SignatureChoice: Sendable { case keepText, append }
    public struct Decision: Sendable {
        public let id: UUID
        public let identity: ComposeIdentity
    }
    public struct Transition: Sendable {
        public let previousIdentityId: String?
        public let identityId: String?
        public let previousBody: String
        public let body: String
        public let format: ComposeFormat
    }

    public private(set) var catalog: ComposeCatalog?
    public private(set) var readState = ReadState.unloaded
    public private(set) var pendingDecision: Decision?
    public private(set) var problem: String?
    /// Synchronous: the shell must not leave an event-loop gap between route,
    /// native body, strip and title publication. Called only after canApply.
    public var onTransition: (@MainActor (Transition) -> Void)?

    private let draft: DraftModel
    private let source: any ComposeCatalogSource
    private let preferredAccountId: String?
    private let canApply: @MainActor () -> Bool
    private var stopped = false
    private var explicitChoice = false
    private var defaultDecided = false
    private var catalogEpoch: UInt64 = 0
    private var readTicket: UInt64 = 0
    @ObservationIgnored private var loading: (id: UInt64, task: Task<Void, Never>)?
    private var decisionSnapshot: (version: UInt64, catalog: UInt64)?
    private var ownedSignature: (prefix: String, body: String, version: UInt64)?

    public init(
        draft: DraftModel, source: any ComposeCatalogSource, preferredAccountId: String? = nil,
        canApply: @escaping @MainActor () -> Bool = { true }
    ) {
        self.draft = draft
        self.source = source
        self.preferredAccountId = preferredAccountId
        self.canApply = canApply
    }

    deinit { loading?.task.cancel() }

    public var choices: [ComposeIdentity] { catalog?.identities.filter(Self.isSelectable) ?? [] }
    public var selectedIdentity: ComposeIdentity? {
        guard let id = draft.content?.identityId else { return nil }
        return catalog?.identities.first { Self.same($0.identityId, id) }
    }
    public var isReady: Bool {
        if case .loaded = readState { return !stopped }
        return false
    }

    /// A query waiter is owned even if its caller is cancelled. Replacement and
    /// retirement cancel and await the entire old chain before opening another.
    public func reload() async {
        guard !stopped else { return }
        let previous = loading
        previous?.task.cancel()
        readTicket &+= 1
        let ticket = readTicket
        readState = .loading
        let source = source
        let preferred = preferredAccountId
        let task = Task { [weak self] in
            await previous?.task.value
            guard !Task.isCancelled else { return }
            do {
                let answer = try await source.composeCatalog(preferredAccountId: preferred)
                guard let self, !self.stopped, self.readTicket == ticket, !Task.isCancelled else { return }
                guard Set(answer.identities.map { Data($0.identityId.utf8) }).count == answer.identities.count
                else {
                    throw CoreFailure.unavailable(
                        reason: "The sending identity catalog contains duplicate identifiers.")
                }
                self.catalog = answer
                self.catalogEpoch &+= 1
                self.readState = .loaded
                self.applyDefaultIfNeeded()
            } catch {
                guard let self, !self.stopped, self.readTicket == ticket, !Task.isCancelled else { return }
                // The last complete catalog stays explicitly stale, never a
                // successful empty list that silently hides the saved route.
                self.readState = .failed(error)
            }
        }
        loading = (ticket, task)
        await task.value
        if loading?.id == ticket { loading = nil }
    }

    @discardableResult
    public func choose(identityId: String?) -> Change {
        guard editable else { return refuse("This draft cannot change its sending identity.") }
        explicitChoice = true
        pendingDecision = nil
        decisionSnapshot = nil
        if Self.same(draft.content?.identityId, identityId) {
            problem = nil
            return .unchanged
        }
        let identity: ComposeIdentity?
        if let identityId {
            guard isReady, let found = choices.first(where: { Self.same($0.identityId, identityId) }) else {
                return refuse("Choose an available sending identity, or keep this draft without one.")
            }
            identity = found
        } else {
            identity = nil
        }
        return propose(identity)
    }

    @discardableResult
    public func resolve(decisionId: UUID, choice: SignatureChoice) -> Change {
        guard let decision = pendingDecision, decision.id == decisionId, let snapshot = decisionSnapshot
        else {
            return refuse("Choose the sending identity again; this signature decision is no longer current.")
        }
        pendingDecision = nil
        decisionSnapshot = nil
        guard editable, snapshot.version == draft.editVersion, snapshot.catalog == catalogEpoch,
            isReady, choices.contains(where: { Self.same($0.identityId, decision.identity.identityId) })
        else {
            return refuse(
                "The draft or its sending identities changed. Choose From again without replacing your text.")
        }
        guard let content = draft.content else { return .refused }
        switch choice {
        case .keepText:
            return apply(identity: decision.identity, body: content.bodySource, ownershipPrefix: nil)
        case .append:
            let body = Self.appending(decision.identity.signatureSource, to: content.bodySource)
            return apply(identity: decision.identity, body: body, ownershipPrefix: content.bodySource)
        }
    }

    public func cancelDecision() {
        pendingDecision = nil
        decisionSnapshot = nil
    }

    /// Native undo restores only the route; the native text undo restores body.
    /// Keep every later recipient/attachment edit and relinquish signature ownership.
    @discardableResult
    public func restoreRoute(identityId: String?) -> Bool {
        guard admitsChange(), var content = draft.content else { return false }
        explicitChoice = true
        cancelDecision()
        ownedSignature = nil
        content.identityId = identityId
        draft.update(content)
        return true
    }

    public func stopAndWait() async {
        stopped = true
        readTicket &+= 1
        cancelDecision()
        let previous = loading
        loading = nil
        previous?.task.cancel()
        await previous?.task.value
        readState = .stopped
    }

    private var editable: Bool {
        !stopped && !draft.isStopped && !draft.isDiscarded && draft.draft?.state == .editing
            && draft.content != nil
    }

    private func applyDefaultIfNeeded() {
        guard !defaultDecided, !explicitChoice, editable else { return }
        defaultDecided = true
        // A saved nil route is intentional, including a receipt arriving while
        // the catalog was loading. Browsing never reroutes a resumed draft.
        guard draft.baseRevisionId == nil, draft.content?.identityId == nil,
            let id = catalog?.defaultIdentityId,
            let identity = choices.first(where: { Self.same($0.identityId, id) })
        else { return }
        // A late default never appends to text typed while metadata was loading.
        guard let content = draft.content else { return }
        if content.bodySource.isEmpty {
            _ = propose(identity)
        } else {
            _ = apply(identity: identity, body: content.bodySource, ownershipPrefix: nil)
        }
    }

    private func propose(_ identity: ComposeIdentity?) -> Change {
        guard let content = draft.content else { return .refused }
        if let owned = ownedSignature, owned.version == draft.editVersion,
            owned.body.utf8.elementsEqual(content.bodySource.utf8)
        {
            return apply(
                identity: identity, body: Self.appending(identity?.signatureSource, to: owned.prefix),
                ownershipPrefix: owned.prefix)
        }
        ownedSignature = nil
        if content.bodySource.isEmpty {
            return apply(
                identity: identity, body: Self.appending(identity?.signatureSource, to: ""),
                ownershipPrefix: "")
        }
        guard let identity, let signature = identity.signatureSource, !signature.isEmpty else {
            return apply(identity: identity, body: content.bodySource, ownershipPrefix: nil)
        }
        let decision = Decision(id: UUID(), identity: identity)
        pendingDecision = decision
        decisionSnapshot = (draft.editVersion, catalogEpoch)
        problem = nil
        return .needsSignatureChoice
    }

    private func admitsChange() -> Bool {
        guard editable else { return false }
        let version = draft.editVersion
        // A native admission callback can commit input reentrantly. Never
        // overwrite that newer source with the already-computed proposal.
        return canApply() && editable && draft.editVersion == version
    }

    private func apply(identity: ComposeIdentity?, body: String, ownershipPrefix: String?) -> Change {
        guard admitsChange(), var content = draft.content else {
            return refuse("Finish the current native input before changing From.")
        }
        let transition = Transition(
            previousIdentityId: content.identityId, identityId: identity?.identityId,
            previousBody: content.bodySource, body: body, format: content.format)
        content.identityId = identity?.identityId
        content.bodySource = body
        draft.update(content)
        if let prefix = ownershipPrefix, let signature = identity?.signatureSource, !signature.isEmpty {
            ownedSignature = (prefix, body, draft.editVersion)
        } else {
            ownedSignature = nil
        }
        problem = nil
        onTransition?(transition)
        return .applied
    }

    private func refuse(_ message: String) -> Change {
        problem = message
        return .refused
    }
    private static func isSelectable(_ identity: ComposeIdentity) -> Bool {
        identity.accountKind == .imap && identity.sendable != .unavailable
    }
    private static func appending(_ signature: String?, to body: String) -> String {
        guard let signature, !signature.isEmpty else { return body }
        return body.isEmpty ? signature : body + "\n\n" + signature
    }
    private static func same(_ lhs: String?, _ rhs: String?) -> Bool {
        switch (lhs, rhs) {
        case (.none, .none): return true
        case (.some(let a), .some(let b)): return a.utf8.elementsEqual(b.utf8)
        default: return false
        }
    }
}
