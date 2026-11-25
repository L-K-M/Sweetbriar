import AppKit
import SweetbriarBridge

/// What each compose cue colors, shared by compose windows and their
/// previews in Settings (spec/08 "Identity strip and account colors").
@MainActor
enum ComposeCuePresentation {
    /// WCAG 2.2 SC 1.4.11, for the monogram against its surface.
    private static let graphicRatio = 3.0

    /// Only Strong and Color Wash tint the window, and only for a resolved
    /// identity. Color Wash draws as Strong until its wash lands (plan
    /// M6.6c).
    static func windowTint(
        mode: ComposeCueMode?, content: IdentityStripView.Content, appearance: NSAppearance
    ) -> NSColor? {
        guard tintsWindow(mode), case .identity(_, _, let tokens) = content else { return nil }
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        return appearanceColor(dark ? tokens.dark.stripSrgb : tokens.light.stripSrgb)
    }

    /// The modes that tint the window behind the strip.
    static func tintsWindow(_ mode: ComposeCueMode?) -> Bool {
        mode == .strong || mode == .wash
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
