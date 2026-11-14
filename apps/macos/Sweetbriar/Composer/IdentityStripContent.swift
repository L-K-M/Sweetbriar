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
        let name = identity.displayName.isEmpty ? "" : "\(identity.displayName) "
        let provisional = identity.sendable == .provisional ? " (provisional)" : ""
        let label = "\(identity.accountName) · \(name)<\(identity.email)>\(provisional)"
        guard identity.sendable != .unavailable else {
            return .unavailable(label: "Unavailable identity: \(label)")
        }
        guard let appearance,
            let resolved = appearance.identities.first(where: {
                sameBytes($0.identityId, route) && sameBytes($0.accountId, identity.accountId)
            })
        else { return .unavailable(label: "Appearance unavailable: \(label)") }
        return .identity(label: label, monogram: resolved.monogram, tokens: resolved.tokens)
    }

    private static func sameBytes(_ lhs: String, _ rhs: String) -> Bool {
        lhs.utf8.elementsEqual(rhs.utf8)
    }
}
