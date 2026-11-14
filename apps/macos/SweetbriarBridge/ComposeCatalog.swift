// Owned composer metadata (spec/07). Generated records stay inside the bridge.
import SweetbriarCore

/// Retained identity metadata, independent of its appearance tokens.
public struct ComposeIdentity: Sendable, Equatable {
    public var identityId: String
    public var accountId: String
    public var accountName: String
    public var accountKind: AccountKind
    public var email: String
    public var displayName: String
    public var replyTo: String?
    public var signatureSource: String?
    public var isDefault: Bool
    public var sendable: IdentitySendable
    public var revision: Int64

    public init(
        identityId: String, accountId: String, accountName: String, accountKind: AccountKind,
        email: String, displayName: String, replyTo: String?, signatureSource: String?,
        isDefault: Bool, sendable: IdentitySendable, revision: Int64
    ) {
        self.identityId = identityId
        self.accountId = accountId
        self.accountName = accountName
        self.accountKind = accountKind
        self.email = email
        self.displayName = displayName
        self.replyTo = replyTo
        self.signatureSource = signatureSource
        self.isDefault = isDefault
        self.sendable = sendable
        self.revision = revision
    }

    init(_ value: SweetbriarCore.ComposeIdentity) {
        let kind: AccountKind =
            switch value.accountKind {
            case .local: .local
            case .imap: .imap
            }
        let availability: IdentitySendable =
            switch value.sendable {
            case .validated: .validated
            case .provisional: .provisional
            case .unavailable: .unavailable
            }
        self.init(
            identityId: value.identityId, accountId: value.accountId, accountName: value.accountName,
            accountKind: kind, email: value.email, displayName: value.displayName, replyTo: value.replyTo,
            signatureSource: value.signatureSource, isDefault: value.isDefault,
            sendable: availability, revision: value.revision)
    }
}

/// A whole identity catalog and its explicitly resolved new-draft default.
public struct ComposeCatalog: Sendable, Equatable {
    public var identities: [ComposeIdentity]
    public var defaultIdentityId: String?

    public init(identities: [ComposeIdentity], defaultIdentityId: String?) {
        self.identities = identities
        self.defaultIdentityId = defaultIdentityId
    }

    init(_ value: SweetbriarCore.ComposeCatalog) {
        self.init(
            identities: value.identities.map(ComposeIdentity.init), defaultIdentityId: value.defaultIdentityId
        )
    }
}

/// Metadata only; a query failure never invents an empty catalog or route.
public protocol ComposeCatalogSource: AnyObject, Sendable {
    func composeCatalog(preferredAccountId: String?) async throws -> ComposeCatalog
}

/// Owned whole-answer parsing and history, independent of AppKit token fields.
public protocol RecipientSource: AnyObject, Sendable {
    func parseDraftRecipients(text: String) async throws -> [DraftRecipient]
    func recipientCompletions(prefix: String, limit: UInt32) async throws -> [DraftRecipient]
}

extension CoreBridge: ComposeCatalogSource, RecipientSource {}
