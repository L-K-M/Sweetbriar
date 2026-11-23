import SweetbriarBridge

/// Resolves the displayed route from the same metadata and appearance snapshots
/// used by the composer. A missing or mismatched snapshot is not a color choice.
@MainActor
enum IdentityStripContent {
    static func resolve(
        route: String?, identity: ComposeIdentity?, appearance: Appearance?
    ) -> IdentityStripView.Content {
        guard let route else { return .unavailable(label: "Without a sending identity") }
        guard let identity, sameBytes(route, identity.identityId) else {
            return .unavailable(label: "Unavailable identity: \(route)")
        }
        let label = label(for: identity)
        guard identity.sendable != .unavailable else {
            return .unavailable(label: "Unavailable identity: \(label)")
        }
        guard let resolved = matchingAppearance(for: identity, in: appearance) else {
            return .unavailable(label: "Appearance unavailable: \(label)")
        }
        return .identity(label: label, monogram: resolved.monogram, tokens: resolved.tokens)
    }

    static func label(for identity: ComposeIdentity) -> String {
        let name = identity.displayName.isEmpty ? "" : "\(identity.displayName) "
        let provisional = identity.sendable == .provisional ? " (provisional)" : ""
        return "\(identity.accountName) · \(name)<\(identity.email)>\(provisional)"
    }

    /// The compose window's title (spec/08 "The window title"): the
    /// sending account first, so the Window menu, Mission Control and
    /// Stage Manager show it.
    static func windowTitle(subject: String, identity: ComposeIdentity?) -> String {
        let subject = subject.isEmpty ? "New Message" : subject
        guard let identity else { return subject }
        return "\(identity.accountName) · \(subject) · \(identity.email)"
    }

    static func matchingAppearance(for identity: ComposeIdentity, in appearance: Appearance?)
        -> IdentityAppearance?
    {
        appearance?.identities.first {
            sameBytes($0.identityId, identity.identityId) && sameBytes($0.accountId, identity.accountId)
        }
    }

    private static func sameBytes(_ lhs: String, _ rhs: String) -> Bool {
        lhs.utf8.elementsEqual(rhs.utf8)
    }
}
