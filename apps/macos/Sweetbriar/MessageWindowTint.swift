import AppKit
import SweetbriarBridge

/// A message window's tint (spec/08 Message window "Tint"): in Color Wash
/// and Strong, the receiving account's strip fill behind the title bar and
/// the reader's header area, with the header's labels in the strip
/// foreground. The fill is opaque and solved for contrast, so Reduce
/// Transparency changes nothing; Increase Contrast adds the outline. The
/// message body keeps the reader's own surface, so Color Wash draws no
/// wash here.
struct MessageWindowTint: Equatable {
    let fill: NSColor
    let foreground: NSColor
    /// The header area's outline, with Increase Contrast.
    let outline: NSColor?

    /// The tint for the account's tokens, or nil in Standard and Minimal,
    /// before the mode is read and without the account's appearance.
    @MainActor
    static func resolve(
        mode: ComposeCueMode?, tokens: AppearanceTokens?, appearance: NSAppearance,
        increaseContrast: Bool
    ) -> MessageWindowTint? {
        guard ComposeCuePresentation.tintsWindow(mode), let tokens else { return nil }
        let dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        let variant = dark ? tokens.dark : tokens.light
        return MessageWindowTint(
            fill: appearanceColor(variant.stripSrgb),
            foreground: appearanceColor(variant.foregroundSrgb),
            outline: increaseContrast ? appearanceColor(variant.outlineSrgb) : nil)
    }
}

/// The reader's backdrop. Neutral, it draws nothing; tinted, it draws the
/// header area, from the top edge under the title bar to just below the
/// header's controls, in the fill over the window background.
final class ReaderBackdropView: NSView {
    var tint: MessageWindowTint? {
        didSet { if tint != oldValue { needsDisplay = true } }
    }
    /// The view whose bottom edge ends the header area.
    weak var headerEnd: NSView?
    /// Told when Dark Mode or another appearance change resolves the
    /// tokens differently.
    var appearanceDidChange: (() -> Void)?

    /// The header area in this view's coordinates; empty while the
    /// header is hidden.
    var headerRect: NSRect {
        guard let headerEnd, !headerEnd.isHidden else { return .zero }
        let end = convert(headerEnd.bounds, from: headerEnd)
        let bottom = max(0, end.minY - 6)
        return NSRect(x: 0, y: bottom, width: bounds.width, height: max(0, bounds.maxY - bottom))
    }

    override func layout() {
        super.layout()
        if tint != nil { needsDisplay = true }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        appearanceDidChange?()
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        guard let tint else { return }
        NSColor.windowBackgroundColor.setFill()
        bounds.fill()
        let header = headerRect
        guard !header.isEmpty else { return }
        tint.fill.setFill()
        header.fill()
        if let outline = tint.outline {
            outline.setStroke()
            let path = NSBezierPath(rect: header.insetBy(dx: 0.75, dy: 0.75))
            path.lineWidth = 1.5
            path.stroke()
        }
    }
}
