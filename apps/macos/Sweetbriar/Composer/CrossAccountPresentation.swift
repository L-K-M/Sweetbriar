import SweetbriarBridge

/// The cross-account banner's and Send confirmation's words (spec/08
/// "Cross-account check"): the account that received the original, and
/// the sending identity with its account and full address.
@MainActor
enum CrossAccountPresentation {
    /// The sending identity as the strip names it, when it belongs to the
    /// route's account; the account's name alone otherwise.
    static func sender(routeAccountId: String, routeAccountName: String, identity: ComposeIdentity?)
        -> String
    {
        guard let identity, identity.accountId.utf8.elementsEqual(routeAccountId.utf8) else {
            return routeAccountName
        }
        return IdentityStripContent.label(for: identity)
    }

    static func banner(sender: String, sourceAccountName: String) -> String {
        "This reply goes out from \(sender), but \(sourceAccountName) received the original."
    }

    static func confirmation(routeAccountName: String, sender: String, sourceAccountName: String)
        -> (message: String, information: String)
    {
        (
            "Send from \(routeAccountName)?",
            "You're sending as \(sender). The original arrived at \(sourceAccountName)."
        )
    }
}
