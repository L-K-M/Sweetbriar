import SwiftUI
import SweetbriarBridge

/// What an account chip shows (spec/08 "Account chips"): the label, and
/// the account's monogram and colors for the current appearance. Without
/// the account's appearance it is neutral: the label alone.
struct AccountChipPresentation: Equatable {
    struct Colors: Equatable {
        let strip: String
        let marker: String
        let foreground: String
        let outline: String
    }

    let label: String
    let monogram: String?
    let colors: Colors?

    @MainActor
    static func make(label: String, accountId: String, appearance: Appearance?, dark: Bool) -> Self {
        guard
            let account = appearance?.accounts.first(where: { $0.accountId.utf8.elementsEqual(accountId.utf8) })
        else {
            return Self(label: label, monogram: nil, colors: nil)
        }
        let variant = dark ? account.tokens.dark : account.tokens.light
        return Self(
            label: label, monogram: account.monogram.isEmpty ? nil : account.monogram,
            colors: Colors(
                strip: variant.stripSrgb, marker: variant.markerSrgb,
                foreground: variant.foregroundSrgb, outline: variant.outlineSrgb))
    }
}

/// An account chip: the monogram in the marker color and the label in the
/// foreground color on the strip fill (spec/08 "Account chips"). It is one
/// accessibility element with its label, so the account is never named
/// by color alone.
struct AccountChip: View {
    let label: String
    let accountId: String
    let appearance: Appearance?
    /// The element's accessibility identifier; empty for none.
    var identifier = ""

    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiate

    var body: some View {
        let chip = AccountChipPresentation.make(
            label: label, accountId: accountId, appearance: appearance, dark: colorScheme == .dark)
        HStack(spacing: 5) {
            if let colors = chip.colors, let monogram = chip.monogram {
                Text(monogram).bold().foregroundStyle(Color(nsColor: appearanceColor(colors.marker)))
            }
            Text(chip.label)
                .foregroundStyle(chip.colors.map { Color(nsColor: appearanceColor($0.foreground)) } ?? .secondary)
        }
        .padding(.horizontal, chip.colors == nil ? 0 : 6)
        .padding(.vertical, chip.colors == nil ? 0 : 2)
        .background {
            if let colors = chip.colors {
                RoundedRectangle(cornerRadius: 4).fill(Color(nsColor: appearanceColor(colors.strip)))
            }
        }
        .overlay {
            if let colors = chip.colors, contrast == .increased || differentiate {
                RoundedRectangle(cornerRadius: 4)
                    .stroke(Color(nsColor: appearanceColor(colors.outline)), lineWidth: 1)
            }
        }
        .fixedSize()
        .accessibilityElement(children: .ignore)
        .accessibilityLabel(Text(verbatim: chip.label))
        .accessibilityIdentifier(identifier)
    }
}
