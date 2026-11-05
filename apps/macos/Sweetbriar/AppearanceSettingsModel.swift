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
        guard !saving, appearanceFailure == nil else { return }
        guard appearance.appearance?.accounts.first(where: { $0.accountId == account.accountId })?.revision == account.revision else {
            error = "The appearance changed in another window. Choose a color again."
            return
        }
        switch (color, account.assignment) {
        case (.paletteV1(let selected), .paletteV1(let current)) where selected == current: return
        case (.custom(let selected), .custom) where selected == account.colorSrgb: return
        default: break
        }
        saving = true
        error = nil
        defer { saving = false }
        do {
            _ = try await appearance.setAccountAppearance(
                accountId: account.accountId, expectedRevision: account.revision,
                change: .init(color: color, symbol: account.symbol)
            )
            await appearance.refresh()
        } catch let failure as CoreFailure {
            if case .conflict = failure {
                await appearance.refresh()
                error = "The appearance changed in another window. Choose a color again."
            } else {
                error = failure.localizedDescription
            }
        } catch is CancellationError {
            await Task { await appearance.refresh() }.value
            error = "The save was interrupted. Check the current color before trying again."
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
