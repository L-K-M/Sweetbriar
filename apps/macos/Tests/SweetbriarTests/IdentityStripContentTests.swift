import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@Suite("Identity strip content") @MainActor
struct IdentityStripContentTests {
    private func identity(_ id: String = "work") -> ComposeIdentity {
        ComposeIdentity(
            identityId: id, accountId: "account", accountName: "Work", accountKind: .imap,
            email: "alex@EXAMPLE.test", displayName: "Alex Example", replyTo: nil,
            signatureSource: nil, isDefault: false, sendable: .validated, revision: 1)
    }

    private func snapshot(id: String = "work", account: String = "account") -> Appearance {
        Appearance(
            generation: 1, accounts: [],
            identities: [
                IdentityAppearance(
                    identityId: id, accountId: account, colorSrgb: "#123456", colorOverrideSrgb: nil,
                    tokens: sampleAccount.tokens, symbolOverride: nil, monogram: "W", revision: nil)
            ])
    }

    @Test func resolvedIdentityUsesFullMetadataAndCoreTokens() {
        let result = IdentityStripContent.resolve(route: "work", identity: identity(), appearance: snapshot())
        #expect(
            result
                == .identity(
                    label: "Work · Alex Example <alex@EXAMPLE.test>", monogram: "W",
                    tokens: sampleAccount.tokens))
        var provisional = identity()
        provisional.sendable = .provisional
        #expect(
            IdentityStripContent.resolve(route: "work", identity: provisional, appearance: snapshot())
                == .identity(
                    label: "Work · Alex Example <alex@EXAMPLE.test> (provisional)", monogram: "W",
                    tokens: sampleAccount.tokens))
        provisional.displayName = ""
        #expect(
            IdentityStripContent.resolve(route: "work", identity: provisional, appearance: snapshot())
                == .identity(
                    label: "Work · <alex@EXAMPLE.test> (provisional)", monogram: "W",
                    tokens: sampleAccount.tokens))
    }

    @Test func missingOrUnavailableDataNeverGuessesColor() {
        #expect(
            IdentityStripContent.resolve(route: nil, identity: identity(), appearance: snapshot())
                == .unavailable(label: "Without a sending identity"))
        #expect(
            IdentityStripContent.resolve(route: "work", identity: nil, appearance: snapshot())
                == .unavailable(label: "Unavailable identity: work"))
        #expect(
            IdentityStripContent.resolve(route: "work", identity: identity(), appearance: nil)
                == .unavailable(label: "Appearance unavailable: Work · Alex Example <alex@EXAMPLE.test>"))
        #expect(
            IdentityStripContent.resolve(
                route: "work", identity: identity(), appearance: snapshot(account: "other"))
                == .unavailable(label: "Appearance unavailable: Work · Alex Example <alex@EXAMPLE.test>"))
        var unavailable = identity()
        unavailable.sendable = .unavailable
        #expect(
            IdentityStripContent.resolve(route: "work", identity: unavailable, appearance: snapshot())
                == .unavailable(label: "Unavailable identity: Work · Alex Example <alex@EXAMPLE.test>"))
    }

    @Test func routeAndAccountMatchingUsesExactBytes() {
        let composed = "é"
        let decomposed = "e\u{301}"
        #expect(composed == decomposed)
        #expect(
            IdentityStripContent.resolve(route: composed, identity: identity(decomposed), appearance: nil)
                == .unavailable(label: "Unavailable identity: \(composed)"))
        #expect(
            IdentityStripContent.resolve(
                route: composed, identity: identity(composed), appearance: snapshot(id: decomposed))
                == .unavailable(label: "Appearance unavailable: Work · Alex Example <alex@EXAMPLE.test>"))
        var person = identity()
        person.accountId = composed
        #expect(
            IdentityStripContent.resolve(
                route: "work", identity: person, appearance: snapshot(account: decomposed))
                == .unavailable(label: "Appearance unavailable: Work · Alex Example <alex@EXAMPLE.test>"))
    }

    /// spec/08 "The window title": the account leads, so the Window menu
    /// and Mission Control name it first.
    @Test func theWindowTitleStartsWithTheSendingAccount() {
        #expect(
            IdentityStripContent.windowTitle(subject: "Lunch", identity: identity())
                == "Work · Lunch · alex@EXAMPLE.test")
        #expect(
            IdentityStripContent.windowTitle(subject: "", identity: identity())
                == "Work · New Message · alex@EXAMPLE.test")
        #expect(IdentityStripContent.windowTitle(subject: "Lunch", identity: nil) == "Lunch")
        #expect(IdentityStripContent.windowTitle(subject: "", identity: nil) == "New Message")
    }

    /// spec/08 "Cross-account check": both texts name the receiving
    /// account and the sending identity with its full address.
    @Test func theCrossAccountWarningNamesTheSendingIdentity() {
        let sender = CrossAccountPresentation.sender(
            routeAccountId: "account", routeAccountName: "Work", identity: identity())
        #expect(sender == "Work · Alex Example <alex@EXAMPLE.test>")
        #expect(
            CrossAccountPresentation.banner(sender: sender, sourceAccountName: "Personal")
                == "This reply goes out from Work · Alex Example <alex@EXAMPLE.test>, but Personal received the original.")
        let confirmation = CrossAccountPresentation.confirmation(
            routeAccountName: "Work", sender: sender, sourceAccountName: "Personal")
        #expect(confirmation.message == "Send from Work?")
        #expect(
            confirmation.information
                == "You're sending as Work · Alex Example <alex@EXAMPLE.test>. The original arrived at Personal.")
    }

    /// An identity of another account than the route's never stands in
    /// for the sender: the account's name is all that's known.
    @Test func anIdentityOfAnotherAccountIsNotNamedAsTheSender() {
        #expect(
            CrossAccountPresentation.sender(
                routeAccountId: "other", routeAccountName: "Other", identity: identity()) == "Other")
        #expect(
            CrossAccountPresentation.sender(routeAccountId: "account", routeAccountName: "Work", identity: nil)
                == "Work")
    }
}
