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
            let account = appearance?.accounts.first(where: { sameBytes($0.accountId, accountId) })
        else {
            return Self(label: label, monogram: nil, colors: nil)
        }
        return Self(label: label, monogram: account.monogram, tokens: account.tokens, dark: dark)
    }

    /// An identity chip: the identity's own monogram and colors, matched by
    /// both ids as the identity strip matches them.
    @MainActor
    static func make(
        label: String, identityId: String, accountId: String, appearance: Appearance?, dark: Bool
    ) -> Self {
        guard
            let identity = appearance?.identities.first(where: {
                sameBytes($0.identityId, identityId) && sameBytes($0.accountId, accountId)
            })
        else {
            return Self(label: label, monogram: nil, colors: nil)
        }
        return Self(label: label, monogram: identity.monogram, tokens: identity.tokens, dark: dark)
    }

    init(label: String, monogram: String?, colors: Colors?) {
        self.label = label
        self.monogram = monogram
        self.colors = colors
    }

    private init(label: String, monogram: String, tokens: AppearanceTokens, dark: Bool) {
        let variant = dark ? tokens.dark : tokens.light
        self.init(
            label: label, monogram: monogram.isEmpty ? nil : monogram,
            colors: Colors(
                strip: variant.stripSrgb, marker: variant.markerSrgb,
                foreground: variant.foregroundSrgb, outline: variant.outlineSrgb))
    }

    private static func sameBytes(_ lhs: String, _ rhs: String) -> Bool {
        lhs.utf8.elementsEqual(rhs.utf8)
    }
}

/// An account chip: the monogram in the marker color and the label in the
/// foreground color on the strip fill (spec/08 "Account chips"). It is one
/// accessibility element with its label, so the account is never named
/// by color alone.
struct AccountChip: View {
    /// Whose monogram and colors the chip shows.
    enum Owner: Equatable {
        case account(String)
        case identity(id: String, accountId: String)
    }

    let label: String
    let owner: Owner
    let appearance: Appearance?
    /// The element's accessibility identifier; empty for none.
    var identifier = ""

    init(label: String, accountId: String, appearance: Appearance?, identifier: String = "") {
        self.init(label: label, owner: .account(accountId), appearance: appearance, identifier: identifier)
    }

    init(label: String, owner: Owner, appearance: Appearance?, identifier: String = "") {
        self.label = label
        self.owner = owner
        self.appearance = appearance
        self.identifier = identifier
    }

    @Environment(\.colorScheme) private var colorScheme
    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiate

    var body: some View {
        let dark = colorScheme == .dark
        let chip =
            switch owner {
            case .account(let accountId):
                AccountChipPresentation.make(
                    label: label, accountId: accountId, appearance: appearance, dark: dark)
            case .identity(let id, let accountId):
                AccountChipPresentation.make(
                    label: label, identityId: id, accountId: accountId, appearance: appearance, dark: dark)
            }
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
