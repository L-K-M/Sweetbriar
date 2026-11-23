import AppKit
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@MainActor
func swatchBytes(_ image: NSImage) throws -> [UInt8] {
    let space = try #require(CGColorSpace(name: CGColorSpace.sRGB))
    var bytes = [UInt8](repeating: 0, count: 36 * 36 * 4)
    try bytes.withUnsafeMutableBytes { buffer in
        let context = try #require(
            CGContext(
                data: buffer.baseAddress, width: 36, height: 36, bitsPerComponent: 8,
                bytesPerRow: 36 * 4, space: space,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue | CGBitmapInfo.byteOrder32Big.rawValue
            ))
        let graphics = NSGraphicsContext(cgContext: context, flipped: false)
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = graphics
        image.draw(in: NSRect(x: 0, y: 0, width: 36, height: 36))
        NSGraphicsContext.restoreGraphicsState()
    }
    return bytes
}

@MainActor
func swatchPixel(_ image: NSImage, x: Double, y: Double) throws -> NSColor {
    let bytes = try swatchBytes(image)
    let column = min(35, Int(36 * x))
    let row = min(35, Int(36 * y))
    let index = (row * 36 + column) * 4
    return NSColor(
        srgbRed: CGFloat(bytes[index]) / 255, green: CGFloat(bytes[index + 1]) / 255,
        blue: CGFloat(bytes[index + 2]) / 255, alpha: CGFloat(bytes[index + 3]) / 255)
}

/// The swatch's marker color: the inner pixel least like the strip fill
/// beside the outline. The monogram's solid strokes, or the marker square
/// without one, show it.
@MainActor
func swatchMarker(_ image: NSImage) throws -> NSColor {
    let bytes = try swatchBytes(image)
    let fill = (3 * 36 + 3) * 4
    var best = fill
    var bestDistance = -1
    for row in 4..<32 {
        for column in 4..<32 {
            let index = (row * 36 + column) * 4
            let distance = (0..<3).map { abs(Int(bytes[index + $0]) - Int(bytes[fill + $0])) }.reduce(0, +)
            if distance > bestDistance { (best, bestDistance) = (index, distance) }
        }
    }
    return NSColor(
        srgbRed: CGFloat(bytes[best]) / 255, green: CGFloat(bytes[best + 1]) / 255,
        blue: CGFloat(bytes[best + 2]) / 255, alpha: CGFloat(bytes[best + 3]) / 255)
}

@Suite("Identity swatches") @MainActor
struct IdentitySwatchTests {
    @Test func tokenSurfacesAreOpaqueAndNeverTemplates() throws {
        for (name, marker, background) in [
            (NSAppearance.Name.aqua, "#0072b2", "#ffffff"),
            (NSAppearance.Name.darkAqua, "#56b4e9", "#000000"),
        ] {
            let appearance = try #require(NSAppearance(named: name))
            let key = IdentitySwatch.Key(tokens: sampleAccount.tokens, monogram: "", appearance: appearance)
            let image = IdentitySwatch.image(key: key)
            #expect(!image.isTemplate)
            let center = try swatchPixel(image, x: 0.5, y: 0.5)
            let edge = try swatchPixel(image, x: 0.15, y: 0.5)
            let expectedMarker = try #require(appearanceColor(marker).usingColorSpace(.sRGB))
            let expectedBackground = try #require(appearanceColor(background).usingColorSpace(.sRGB))
            #expect(abs(center.redComponent - expectedMarker.redComponent) < 0.02)
            #expect(abs(center.greenComponent - expectedMarker.greenComponent) < 0.02)
            #expect(abs(center.blueComponent - expectedMarker.blueComponent) < 0.02)
            #expect(abs(edge.redComponent - expectedBackground.redComponent) < 0.02)
            #expect(abs(edge.greenComponent - expectedBackground.greenComponent) < 0.02)
            #expect(abs(edge.blueComponent - expectedBackground.blueComponent) < 0.02)
            #expect(center.alphaComponent == 1 && edge.alphaComponent == 1)
            let bytes = try swatchBytes(image)
            for pixel in 0..<(36 * 36) { #expect(bytes[pixel * 4 + 3] == 255) }
        }
    }

    /// spec/08: the swatch carries the identity's monogram in the marker
    /// color on the strip fill. Every pixel blends only those two colors,
    /// so a color glyph can't bring its own hue.
    @Test(arguments: ["W", "🐝"])
    func theMonogramIsDrawnInTheMarkerOnly(monogram: String) throws {
        let appearance = try #require(NSAppearance(named: .aqua))
        let key = IdentitySwatch.Key(tokens: sampleAccount.tokens, monogram: monogram, appearance: appearance)
        #expect(key != IdentitySwatch.Key(tokens: sampleAccount.tokens, monogram: "", appearance: appearance))
        let image = IdentitySwatch.image(key: key)
        let marker = try #require(appearanceColor("#0072b2").usingColorSpace(.sRGB))
        let background = try #require(appearanceColor("#ffffff").usingColorSpace(.sRGB))
        let bytes = try swatchBytes(image)
        var markerPixels = 0
        // The inner area: the outline sits in the outermost pixels.
        for row in 4..<32 {
            for column in 4..<32 {
                let index = (row * 36 + column) * 4
                let pixel = (0..<3).map { CGFloat(bytes[index + $0]) / 255 }
                let ends = [
                    (marker.redComponent, background.redComponent),
                    (marker.greenComponent, background.greenComponent),
                    (marker.blueComponent, background.blueComponent),
                ]
                // How far toward the marker this pixel is, from its
                // channel with the widest span.
                let (channel, span) = ends.enumerated().map { ($0.offset, $0.element.0 - $0.element.1) }
                    .max { abs($0.1) < abs($1.1) }!
                let share = (pixel[channel] - ends[channel].1) / span
                #expect(share > -0.05 && share < 1.05)
                for (value, end) in zip(pixel, ends) {
                    #expect(abs(value - (end.1 + share * (end.0 - end.1))) < 0.06)
                }
                if share > 0.9 { markerPixels += 1 }
                #expect(bytes[index + 3] == 255)
            }
        }
        #expect(markerPixels > 12, "the monogram is visible")
        // The glyph doesn't fill the swatch: the strip shows around it.
        #expect(markerPixels < 28 * 28 / 2)
    }
}
