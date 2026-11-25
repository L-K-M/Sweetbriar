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
        /// The logo shown in the monogram's place, by account and logo
        /// revision (spec/08 "Logo"); nil for the monogram.
        let logo: String?
        let dark: Bool

        init(tokens: AppearanceTokens, monogram: String, appearance: NSAppearance, logo: String? = nil) {
            let variant =
                appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua ? tokens.dark : tokens.light
            marker = variant.markerSrgb
            background = variant.stripSrgb
            outline = variant.outlineSrgb
            self.monogram = monogram
            self.logo = logo
            dark = appearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
        }
    }

    /// `logo` replaces the monogram on an opaque plate of the window
    /// background, ringed in the marker color.
    static func image(key: Key, logo: NSImage? = nil) -> NSImage {
        let line = key.monogram.isEmpty ? nil : monogramLine(key.monogram)
        var plate = NSColor.windowBackgroundColor.cgColor
        NSAppearance(named: key.dark ? .darkAqua : .aqua)?.performAsCurrentDrawingAppearance {
            plate = NSColor.windowBackgroundColor.cgColor
        }
        let image = drawImage(
            marker: appearanceColor(key.marker).cgColor,
            background: appearanceColor(key.background).cgColor,
            outline: appearanceColor(key.outline).cgColor,
            monogram: line.map(Monogram.init),
            logo: logo?.cgImage(forProposedRect: nil, context: nil, hints: nil).map { Logo(image: $0, plate: plate) })
        // A template image would be recolored when AppKit highlights a row.
        image.isTemplate = false
        return image
    }

    /// A typeset monogram. CTLine is immutable and safe to draw from the
    /// image handler's thread.
    private struct Monogram: @unchecked Sendable {
        let line: CTLine
    }

    /// A decoded logo and its plate; CGImage and CGColor are immutable.
    private struct Logo: @unchecked Sendable {
        let image: CGImage
        let plate: CGColor
    }

    private static func monogramLine(_ monogram: String) -> CTLine {
        let font = NSFont.systemFont(ofSize: 11, weight: .bold)
        return CTLineCreateWithAttributedString(
            NSAttributedString(string: monogram, attributes: [.font: font]))
    }

    // AppKit may invoke an image drawing handler on any thread. Capture only
    // immutable Core Graphics colors and lines, and use the current context.
    private nonisolated static func drawImage(
        marker: CGColor, background: CGColor, outline: CGColor, monogram: Monogram?, logo: Logo?
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
            if let logo {
                let square = CGRect(x: 2, y: 2, width: 14, height: 14)
                let shape = CGPath(roundedRect: square, cornerWidth: 3, cornerHeight: 3, transform: nil)
                context.saveGState()
                context.addPath(shape)
                context.clip()
                context.setFillColor(logo.plate)
                context.fill(square)
                context.interpolationQuality = .high
                context.draw(logo.image, in: square)
                context.restoreGState()
                context.setStrokeColor(marker)
                context.setLineWidth(1)
                context.addPath(
                    CGPath(roundedRect: square.insetBy(dx: 0.5, dy: 0.5), cornerWidth: 3, cornerHeight: 3, transform: nil))
                context.strokePath()
                return true
            }
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
