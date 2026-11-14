import AppKit
import SweetbriarBridge

/// An opaque token-backed sample: the core's marker and outline are contrasted
/// against its own strip surface, not against the menu's selected-row color.
@MainActor
enum IdentitySwatch {
    struct Key: Hashable {
        let marker: String
        let background: String
        let outline: String

        init(tokens: AppearanceTokens, appearance: NSAppearance) {
            let variant =
                appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? tokens.dark : tokens.light
            marker = variant.markerSrgb
            background = variant.stripSrgb
            outline = variant.outlineSrgb
        }
    }

    static func image(key: Key) -> NSImage {
        let image = drawImage(
            marker: appearanceColor(key.marker).cgColor,
            background: appearanceColor(key.background).cgColor,
            outline: appearanceColor(key.outline).cgColor)
        // A template image would be recolored when AppKit highlights a row.
        image.isTemplate = false
        return image
    }

    // AppKit may invoke an image drawing handler on any thread. Capture only
    // immutable Core Graphics colors and use the current drawing context.
    private nonisolated static func drawImage(marker: CGColor, background: CGColor, outline: CGColor)
        -> NSImage
    {
        NSImage(size: NSSize(width: 18, height: 18), flipped: false) { rect in
            guard let context = NSGraphicsContext.current?.cgContext else { return false }
            context.saveGState()
            defer { context.restoreGState() }
            context.translateBy(x: rect.minX, y: rect.minY)
            context.scaleBy(x: rect.width / 18, y: rect.height / 18)
            context.setFillColor(background)
            context.fill(CGRect(x: 0, y: 0, width: 18, height: 18))
            context.setStrokeColor(outline)
            context.setLineWidth(1)
            context.stroke(CGRect(x: 0.5, y: 0.5, width: 17, height: 17))
            context.setFillColor(marker)
            context.fill(CGRect(x: 4, y: 4, width: 10, height: 10))
            return true
        }
    }
}
