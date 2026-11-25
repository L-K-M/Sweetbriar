import AppKit
import Observation
import SweetbriarBridge

/// Keeps edits separate from the shared, committed appearance snapshot.
@MainActor
@Observable
final class AppearanceSettingsModel {
    let appearance: AppearanceModel
    private(set) var saving = false
    private(set) var error: String?
    var appearanceFailure: String? { appearance.lastError?.localizedDescription }

    init(appearance: AppearanceModel) { self.appearance = appearance }

    func retryAppearance() async {
        await appearance.refresh()
        error = nil
    }

    func choose(_ color: AccountColor, account: AccountAppearance) async {
        switch (color, account.assignment) {
        case (.paletteV1(let selected), .paletteV1(let current)) where selected == current: return
        case (.custom(let selected), .custom) where selected == account.colorSrgb: return
        default: break
        }
        await writeAccount(
            account, change: .init(color: color, symbol: account.symbol),
            retry: "The appearance changed in another window. Choose a color again.")
    }

    /// The account's own letter (spec/08 Settings → Appearance "Letter"):
    /// written as the account's `symbol`, the empty field restoring the
    /// automatic letter.
    func setLetter(_ text: String, account: AccountAppearance) async {
        let symbol = Self.letter(text)
        guard symbol != account.symbol else { return }
        await writeAccount(
            account, change: .init(color: Self.color(of: account), symbol: symbol),
            retry: "The appearance changed in another window. Check the letter and enter it again.")
    }

    /// One address's color (spec/08 Settings → Appearance "Addresses"):
    /// `nil` uses the account color, otherwise its own `#rrggbb`. The
    /// identity's symbol override stays.
    func setAddressColor(_ colorSrgb: String?, identity: IdentityAppearance) async {
        guard colorSrgb != identity.colorOverrideSrgb else { return }
        let retry = "The appearance changed in another window. Choose the address's color again."
        let shown = appearance.appearance?.identities.first { $0.identityId == identity.identityId }
        await write(current: shown?.revision == identity.revision, retry: retry) { [appearance] in
            _ = try await appearance.setIdentityAppearance(
                identityId: identity.identityId, expectedRevision: identity.revision,
                change: .init(colorOverrideSrgb: colorSrgb, symbolOverride: identity.symbolOverride))
        }
    }

    /// A letter as typed: surrounding whitespace trimmed and at most two
    /// grapheme clusters; empty is `nil`, the automatic letter.
    static func letter(_ text: String) -> String? {
        let trimmed = text.trimmingCharacters(in: .whitespacesAndNewlines)
        let letter = String(trimmed.prefix(2)).trimmingCharacters(in: .whitespacesAndNewlines)
        return letter.isEmpty ? nil : letter
    }

    /// The account's color as the setter takes it, so a letter change
    /// keeps it.
    static func color(of account: AccountAppearance) -> AccountColor {
        if case .paletteV1(let slot) = account.assignment { return .paletteV1(slot: slot) }
        return .custom(colorSrgb: account.colorSrgb)
    }

    private func writeAccount(_ account: AccountAppearance, change: AccountAppearanceChange, retry: String) async {
        let shown = appearance.appearance?.accounts.first { $0.accountId == account.accountId }
        await write(current: shown?.revision == account.revision, retry: retry) { [appearance] in
            _ = try await appearance.setAccountAppearance(
                accountId: account.accountId, expectedRevision: account.revision, change: change)
        }
    }

    /// One write at the observed revision. A value another window changed
    /// is never overwritten: a stale revision or a `Conflict` reloads and
    /// says so (spec/08).
    private func write(current: Bool, retry: String, _ body: () async throws -> Void) async {
        guard !saving, appearanceFailure == nil else { return }
        guard current else {
            error = retry
            return
        }
        saving = true
        error = nil
        defer { saving = false }
        do {
            try await body()
            await appearance.refresh()
        } catch let failure as CoreFailure {
            if case .conflict = failure {
                await appearance.refresh()
                error = retry
            } else {
                error = failure.localizedDescription
            }
        } catch is CancellationError {
            await Task { await appearance.refresh() }.value
            error = "The save was interrupted. Check the current values before trying again."
        } catch {
            self.error = error.localizedDescription
        }
    }

    /// Convert only opaque, representable colors to canonical sRGB bytes.
    static func canonicalColor(_ color: NSColor) -> String? {
        guard let rgb = color.usingColorSpace(.sRGB), rgb.alphaComponent == 1 else { return nil }
        let values = [rgb.redComponent, rgb.greenComponent, rgb.blueComponent]
        guard values.allSatisfy({ $0.isFinite && (0...1).contains($0) }) else { return nil }
        let bytes = values.map { Int(($0 * 255).rounded()) }
        return String(format: "#%02x%02x%02x", bytes[0], bytes[1], bytes[2])
    }
}
