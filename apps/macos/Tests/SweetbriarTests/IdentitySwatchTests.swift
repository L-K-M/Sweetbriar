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

@Suite("Identity swatches") @MainActor
struct IdentitySwatchTests {
    @Test func tokenSurfacesAreOpaqueAndNeverTemplates() throws {
        for (name, marker, background) in [
            (NSAppearance.Name.aqua, "#0072b2", "#ffffff"),
            (NSAppearance.Name.darkAqua, "#56b4e9", "#000000"),
        ] {
            let appearance = try #require(NSAppearance(named: name))
            let key = IdentitySwatch.Key(tokens: sampleAccount.tokens, appearance: appearance)
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
}
