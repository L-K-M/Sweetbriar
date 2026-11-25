import AppKit
import SweetbriarBridge

/// What each compose cue colors, shared by compose windows and their
/// previews in Settings (spec/08 "Identity strip and account colors").
@MainActor
enum ComposeCuePresentation {
    /// WCAG 2.2 SC 1.4.11, for the monogram against its surface.
    private static let graphicRatio = 3.0

    /// Only Strong and Color Wash tint the window, and only for a resolved
    /// identity: the strip fill behind the title bar and the header.
    static func windowTint(
        mode: ComposeCueMode?, content: IdentityStripView.Content, appearance: NSAppearance
    ) -> NSColor? {
        guard tintsWindow(mode), case .identity(_, _, let tokens) = content else { return nil }
        return appearanceColor(variant(tokens, appearance).stripSrgb)
    }

    /// The modes that tint the window behind the strip.
    static func tintsWindow(_ mode: ComposeCueMode?) -> Bool {
        mode == .strong || mode == .wash
    }

    /// The mode a window draws: with Increase Contrast or Reduce
    /// Transparency on, Color Wash renders as Strong (spec/08 "Fallbacks").
    static func renderedMode(
        _ mode: ComposeCueMode?, increaseContrast: Bool, reduceTransparency: Bool
    ) -> ComposeCueMode? {
        guard mode == .wash, increaseContrast || reduceTransparency else { return mode }
        return .strong
    }

    /// Color Wash's gradient below the header, from the top stop to the
    /// bottom one, for a resolved identity (spec/08 "The wash stops").
    static func wash(
        mode: ComposeCueMode?, content: IdentityStripView.Content, appearance: NSAppearance
    ) -> ComposeWashView.Stops? {
        guard mode == .wash, case .identity(_, _, let tokens) = content else { return nil }
        let variant = variant(tokens, appearance)
        return .init(top: appearanceColor(variant.washTopSrgb), bottom: appearanceColor(variant.washBottomSrgb))
    }

    /// What Send shows when it names the account (spec/08 "Send as the
    /// account").
    struct SendAsAccount: Equatable {
        let title: String
        let accessibilityLabel: String
        let fill: NSColor
        let ink: NSColor
    }

    /// Send names the account in Color Wash and Strong for a resolved
    /// identity; otherwise it is the plain Send.
    static func sendAsAccount(
        mode: ComposeCueMode?, content: IdentityStripView.Content, identity: ComposeIdentity?,
        appearance: NSAppearance
    ) -> SendAsAccount? {
        guard tintsWindow(mode), case .identity(_, _, let tokens) = content, let identity else { return nil }
        let variant = variant(tokens, appearance)
        return .init(
            title: "Send as \(identity.accountName)",
            accessibilityLabel: "Send as \(identity.accountName), \(identity.email)",
            fill: appearanceColor(variant.markerSrgb), ink: appearanceColor(variant.markerInkSrgb))
    }

    private static func variant(_ tokens: AppearanceTokens, _ appearance: NSAppearance) -> AppearanceVariant {
        appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? tokens.dark : tokens.light
    }

    /// The core solves a marker against its strip. On another surface the
    /// shell checks it. Call this in the surface's drawing appearance.
    static func markerNeedsOutline(_ marker: NSColor, on surface: NSColor) -> Bool {
        guard let marker = marker.usingColorSpace(.sRGB), let surface = surface.usingColorSpace(.sRGB)
        else { return true }
        let ink = luminance(marker)
        let ground = luminance(surface)
        return (max(ink, ground) + 0.05) / (min(ink, ground) + 0.05) < graphicRatio
    }

    /// WCAG 2.2 relative luminance: the sRGB transfer curve and Rec. 709 weights.
    private static func luminance(_ color: NSColor) -> Double {
        func linear(_ component: CGFloat) -> Double {
            let value = Double(component)
            return value <= 0.04045 ? value / 12.92 : pow((value + 0.055) / 1.055, 2.4)
        }
        return 0.2126 * linear(color.redComponent) + 0.7152 * linear(color.greenComponent)
            + 0.0722 * linear(color.blueComponent)
    }
}
