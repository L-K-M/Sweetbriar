// Settings → Appearance's letter and address controls (spec/08,
// M6.6d): the letter trims, keeps at most two grapheme clusters and
// clears back to the automatic letter; an address's color writes
// through `set_identity_appearance`; a `Conflict` reloads and never
// overwrites.

import AppKit
import Testing
import SweetbriarBridge
@testable import Sweetbriar

private let letterTokens = AppearanceTokens(
    light: .init(
        markerSrgb: "#0072b2", stripSrgb: "#eaf3ff", foregroundSrgb: "#000000", outlineSrgb: "#000000",
        washTopSrgb: "#daeeff", washBottomSrgb: "#f6fbff", markerInkSrgb: "#ffffff"),
    dark: .init(
        markerSrgb: "#56b4e9", stripSrgb: "#1c2530", foregroundSrgb: "#ffffff", outlineSrgb: "#ffffff",
        washTopSrgb: "#142b3d", washBottomSrgb: "#151b21", markerInkSrgb: "#000000"))

/// One account with a custom color and the letter "W", and one of its
/// identities without overrides.
private actor LetterAddressSource: AppearanceSource {
    struct AccountCall: Equatable { let revision: Int64; let change: AccountAppearanceChange }
    struct IdentityCall: Equatable { let identityId: String; let revision: Int64?; let change: IdentityAppearanceChange }
    private(set) var accountCalls: [AccountCall] = []
    private(set) var identityCalls: [IdentityCall] = []
    private(set) var reads = 0
    /// The next identity write fails with `Conflict`, another window
    /// having changed the record first.
    private var conflictNext = false
    private var account = AccountAppearance(
        accountId: "work", colorSrgb: "#0072b2", tokens: letterTokens, symbol: "W", monogram: "W",
        collides: false, assignment: .custom, revision: 4)
    private var identity = IdentityAppearance(
        identityId: "work-1", accountId: "work", colorSrgb: "#0072b2", colorOverrideSrgb: nil,
        tokens: letterTokens, symbolOverride: "Q", monogram: "Q", revision: 2)

    func conflictOnNextWrite() { conflictNext = true }

    func appearance() async throws -> Appearance {
        reads += 1
        return Appearance(generation: UInt64(reads), accounts: [account], identities: [identity])
    }
    func appearanceEvents(generation: UInt64) async throws -> any AppearanceEventSource {
        throw CoreFailure.unavailable(reason: "Not used by this fixture.")
    }
    func setAccountAppearance(
        accountId: String, expectedRevision: Int64, change: AccountAppearanceChange
    ) async throws -> Receipt {
        accountCalls.append(.init(revision: expectedRevision, change: change))
        account.symbol = change.symbol
        account.monogram = change.symbol ?? "W"
        account.revision += 1
        return .init(command: "set_account_appearance", resultJson: "{}", createdAtMs: 0)
    }
    func setIdentityAppearance(
        identityId: String, expectedRevision: Int64?, change: IdentityAppearanceChange
    ) async throws -> Receipt {
        identityCalls.append(.init(identityId: identityId, revision: expectedRevision, change: change))
        if conflictNext {
            conflictNext = false
            identity.colorOverrideSrgb = "#d55e00"
            identity.revision = (identity.revision ?? 0) + 1
            throw CoreFailure.conflict(detail: "stale")
        }
        identity.colorOverrideSrgb = change.colorOverrideSrgb
        identity.revision = (identity.revision ?? 0) + 1
        return .init(command: "set_identity_appearance", resultJson: "{}", createdAtMs: 0)
    }
    nonisolated func palette() -> [PaletteColor] { [] }
}

@MainActor
private func letterModel() async -> (AppearanceSettingsModel, LetterAddressSource) {
    let source = LetterAddressSource()
    let appearance = AppearanceModel(source: source)
    await appearance.refresh()
    return (AppearanceSettingsModel(appearance: appearance), source)
}

@MainActor
struct AppearanceLetterAddressTests {
    @Test func theLetterTrimsAndKeepsTwoGraphemeClusters() {
        #expect(AppearanceSettingsModel.letter("  ab  ") == "ab")
        #expect(AppearanceSettingsModel.letter("abc") == "ab")
        #expect(AppearanceSettingsModel.letter(" a b") == "a")
        // A family emoji and a flag are one grapheme cluster each.
        #expect(AppearanceSettingsModel.letter("👩‍👩‍👧‍👦🇨🇭x") == "👩‍👩‍👧‍👦🇨🇭")
        #expect(AppearanceSettingsModel.letter("é") == "é")
        #expect(AppearanceSettingsModel.letter("   ") == nil)
        #expect(AppearanceSettingsModel.letter("") == nil)
    }

    /// A letter keeps the account's color; the empty field writes no
    /// symbol, the automatic letter; an unchanged letter writes nothing.
    @Test func theLetterWritesTheSymbolAndClearsToTheAutomaticLetter() async throws {
        let (model, source) = await letterModel()
        var account = try #require(model.appearance.appearance?.accounts.first)
        await model.setLetter(" W ", account: account)
        #expect(await source.accountCalls.isEmpty)

        await model.setLetter("Jo Doe", account: account)
        #expect(
            await source.accountCalls == [
                .init(revision: 4, change: .init(color: .custom(colorSrgb: "#0072b2"), symbol: "Jo"))
            ])
        #expect(model.error == nil)

        account = try #require(model.appearance.appearance?.accounts.first)
        #expect(account.symbol == "Jo")
        await model.setLetter("  ", account: account)
        #expect(
            await source.accountCalls.last
                == .init(revision: 5, change: .init(color: .custom(colorSrgb: "#0072b2"), symbol: nil)))
        #expect(model.appearance.appearance?.accounts.first?.symbol == nil)
    }

    /// An address's color writes through `set_identity_appearance` at the
    /// observed revision and keeps its symbol override; "Use account
    /// color" writes none.
    @Test func anAddressColorWritesThroughTheIdentitySetter() async throws {
        let (model, source) = await letterModel()
        var identity = try #require(model.appearance.appearance?.identities.first)
        await model.setAddressColor("#009e73", identity: identity)
        #expect(
            await source.identityCalls == [
                .init(
                    identityId: "work-1", revision: 2,
                    change: .init(colorOverrideSrgb: "#009e73", symbolOverride: "Q"))
            ])
        identity = try #require(model.appearance.appearance?.identities.first)
        #expect(identity.colorOverrideSrgb == "#009e73")
        await model.setAddressColor(nil, identity: identity)
        #expect(
            await source.identityCalls.last
                == .init(identityId: "work-1", revision: 3, change: .init(colorOverrideSrgb: nil, symbolOverride: "Q")))
        #expect(model.error == nil)
    }

    /// A `Conflict` reloads the values, says so and doesn't retry, so the
    /// other window's change stays; a stale row writes nothing.
    @Test func aConflictReloadsWithoutOverwriting() async throws {
        let (model, source) = await letterModel()
        let identity = try #require(model.appearance.appearance?.identities.first)
        await source.conflictOnNextWrite()
        await model.setAddressColor("#009e73", identity: identity)
        #expect(await source.identityCalls.count == 1)
        #expect(model.error?.contains("changed in another window") == true)
        #expect(model.appearance.appearance?.identities.first?.colorOverrideSrgb == "#d55e00")

        // The row still shows the old revision: nothing is written.
        await model.setAddressColor("#009e73", identity: identity)
        #expect(await source.identityCalls.count == 1)
        #expect(model.error?.contains("changed in another window") == true)
    }
}
