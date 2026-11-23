import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

/// spec/08 "Account chips": the account's monogram and tokens for the
/// current appearance, or a neutral label when its appearance is unknown.
@Suite("Account chips") @MainActor
struct AccountChipTests {
    private func snapshot(_ account: AccountAppearance = sampleAccount) -> Appearance {
        Appearance(generation: 1, accounts: [account], identities: [])
    }

    @Test func aKnownAccountTakesItsMonogramAndTokens() {
        let light = AccountChipPresentation.make(
            label: "Received in Work", accountId: "account-1", appearance: snapshot(), dark: false)
        #expect(light.label == "Received in Work")
        #expect(light.monogram == "W")
        #expect(
            light.colors
                == AccountChipPresentation.Colors(
                    strip: "#ffffff", marker: "#0072b2", foreground: "#000000", outline: "#000000"))
        let dark = AccountChipPresentation.make(
            label: "Work", accountId: "account-1", appearance: snapshot(), dark: true)
        #expect(
            dark.colors
                == AccountChipPresentation.Colors(
                    strip: "#000000", marker: "#56b4e9", foreground: "#ffffff", outline: "#ffffff"))
    }

    @Test func anUnknownAccountOrAppearanceIsNeutral() {
        for appearance in [nil, snapshot()] {
            let chip = AccountChipPresentation.make(
                label: "Received in Other", accountId: "account-2", appearance: appearance, dark: false)
            #expect(chip == AccountChipPresentation(label: "Received in Other", monogram: nil, colors: nil))
        }
        // Ids match by their bytes: a decomposed spelling is another account.
        var composed = sampleAccount
        composed.accountId = "caf\u{e9}"
        let chip = AccountChipPresentation.make(
            label: "Café", accountId: "cafe\u{301}", appearance: snapshot(composed), dark: false)
        #expect(chip.colors == nil)
    }

    @Test func aChipWithoutAMonogramStillTakesTheAccountColors() {
        var plain = sampleAccount
        plain.monogram = ""
        let chip = AccountChipPresentation.make(
            label: "Work", accountId: "account-1", appearance: snapshot(plain), dark: false)
        #expect(chip.monogram == nil)
        #expect(chip.colors?.strip == "#ffffff")
    }

    /// An identity chip takes the identity's own tokens, matched by both
    /// ids; another account's identity of that id is neutral.
    @Test func anIdentityChipTakesTheIdentitysOwnTokens() {
        let tokens = AppearanceTokens(
            light: AppearanceVariant(
                markerSrgb: "#d55e00", stripSrgb: "#fff4ec", foregroundSrgb: "#111111", outlineSrgb: "#222222"),
            dark: sampleAccount.tokens.dark)
        let appearance = Appearance(
            generation: 1, accounts: [sampleAccount],
            identities: [
                IdentityAppearance(
                    identityId: "work", accountId: "account-1", colorSrgb: "#d55e00",
                    colorOverrideSrgb: "#d55e00", tokens: tokens, symbolOverride: "A", monogram: "A",
                    revision: 1)
            ])
        let chip = AccountChipPresentation.make(
            label: "Work · Alex <alex@example.test>", identityId: "work", accountId: "account-1",
            appearance: appearance, dark: false)
        #expect(chip.monogram == "A")
        #expect(chip.colors?.marker == "#d55e00")
        let stranger = AccountChipPresentation.make(
            label: "Other", identityId: "work", accountId: "account-2", appearance: appearance, dark: false)
        #expect(stranger.colors == nil)
    }

    /// spec/08 "Drafts (local)": the From label for each kind of route.
    @Test func draftRowsLabelTheirFromIdentity() {
        let identity = ComposeIdentity(
            identityId: "work", accountId: "account-1", accountName: "Work", accountKind: .imap,
            email: "alex@example.test", displayName: "Alex", replyTo: nil, signatureSource: nil,
            isDefault: true, sendable: .validated, revision: 1)
        let named = DraftRowView.from(.identity(identity))
        #expect(named.label == "Work · Alex <alex@example.test>")
        #expect(named.owner == .identity(id: "work", accountId: "account-1"))
        #expect(DraftRowView.from(.none).label == "No sending identity")
        #expect(DraftRowView.from(.none).owner == nil)
        let gone = DraftRowView.from(.unavailable(identityId: "work"))
        #expect(gone.label == "Sending identity unavailable")
        #expect(gone.owner == nil)
        #expect(DraftRowView.spokenFrom(.identity(identity)) == "From Work · Alex <alex@example.test>")
        #expect(DraftRowView.spokenFrom(.none) == "No sending identity")
        #expect(
            DraftRowView.spokenFrom(.unavailable(identityId: "work")) == "Sending identity unavailable")
    }
}
