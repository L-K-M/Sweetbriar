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
}
