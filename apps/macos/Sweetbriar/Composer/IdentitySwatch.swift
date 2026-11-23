import AppKit
import CoreText
import SweetbriarBridge

/// An opaque token-backed sample: the core's marker and outline are contrasted
/// against its own strip surface, not against the menu's selected-row color.
/// It is the identity strip in small (spec/08 "The From control"): the
/// strip's fill and outline with the monogram in the marker color, or a
/// marker square when there is no monogram.
@MainActor
enum IdentitySwatch {
    struct Key: Hashable {
        let marker: String
        let background: String
        let outline: String
        let monogram: String

        init(tokens: AppearanceTokens, monogram: String, appearance: NSAppearance) {
            let variant =
                appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? tokens.dark : tokens.light
            marker = variant.markerSrgb
            background = variant.stripSrgb
            outline = variant.outlineSrgb
            self.monogram = monogram
        }
    }

    static func image(key: Key) -> NSImage {
        let line = key.monogram.isEmpty ? nil : monogramLine(key.monogram)
        let image = drawImage(
            marker: appearanceColor(key.marker).cgColor,
            background: appearanceColor(key.background).cgColor,
            outline: appearanceColor(key.outline).cgColor,
            monogram: line.map(Monogram.init))
        // A template image would be recolored when AppKit highlights a row.
        image.isTemplate = false
        return image
    }

    /// A typeset monogram. CTLine is immutable and safe to draw from the
    /// image handler's thread.
    private struct Monogram: @unchecked Sendable {
        let line: CTLine
    }

    private static func monogramLine(_ monogram: String) -> CTLine {
        let font = NSFont.systemFont(ofSize: 11, weight: .bold)
        return CTLineCreateWithAttributedString(
            NSAttributedString(string: monogram, attributes: [.font: font]))
    }

    // AppKit may invoke an image drawing handler on any thread. Capture only
    // immutable Core Graphics colors and lines, and use the current context.
    private nonisolated static func drawImage(
        marker: CGColor, background: CGColor, outline: CGColor, monogram: Monogram?
    ) -> NSImage {
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
            guard let monogram else {
                context.setFillColor(marker)
                context.fill(CGRect(x: 4, y: 4, width: 10, height: 10))
                return true
            }
            // Color glyphs (emoji) take the marker too: composite the
            // glyphs' alpha, not their own colors, as the strip does.
            let bounds = CTLineGetBoundsWithOptions(monogram.line, .useGlyphPathBounds)
            let scale = min(1, 12 / max(bounds.width, bounds.height, 1))
            context.clip(to: CGRect(x: 1, y: 1, width: 16, height: 16))
            context.beginTransparencyLayer(auxiliaryInfo: nil)
            context.translateBy(x: 9, y: 9)
            context.scaleBy(x: scale, y: scale)
            context.textMatrix = .identity
            context.textPosition = CGPoint(x: -bounds.midX, y: -bounds.midY)
            CTLineDraw(monogram.line, context)
            context.setBlendMode(.sourceIn)
            context.setFillColor(marker)
            context.fill(CGRect(x: -9 / scale, y: -9 / scale, width: 18 / scale, height: 18 / scale))
            context.endTransparencyLayer()
            return true
        }
    }
}
