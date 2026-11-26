import AppKit
import ImageIO
import SwiftUI
import SweetbriarBridge
import UniformTypeIdentifiers

/// Settings' logo pipeline (spec/08 Settings → Appearance "Logo"): a file
/// of at most 20 MB, decoded by ImageIO, scaled to fill 128 x 128 pixels,
/// cropped to the center and encoded as PNG within the core's 65,536
/// bytes (spec/02 "Account logos").
enum AccountLogoPipeline {
    static let maximumFileBytes = 20 * 1024 * 1024
    static let side = 128
    static let maximumPngBytes = 65_536
    /// ImageIO decodes no more than this on the long side, so a large
    /// file never becomes a full-size bitmap.
    private static let decodeLimit = 1024

    enum Failure: Error, Equatable, LocalizedError {
        case fileTooLarge
        case unreadable
        case tooComplex

        var errorDescription: String? {
            switch self {
            case .fileTooLarge: "Choose an image of 20 MB or less."
            case .unreadable: "Sweetbriar can't read this image."
            case .tooComplex: "Choose a simpler image."
            }
        }
    }

    /// The logo PNG for the image file at `url`. The size check comes
    /// before anything is decoded.
    static func png(fromFileAt url: URL) throws -> Data {
        let size = try? url.resourceValues(forKeys: [.fileSizeKey]).fileSize
        guard let size else { throw Failure.unreadable }
        guard size <= maximumFileBytes else { throw Failure.fileTooLarge }
        guard let source = CGImageSourceCreateWithURL(url as CFURL, nil) else { throw Failure.unreadable }
        let options: [CFString: Any] = [
            kCGImageSourceCreateThumbnailFromImageAlways: true,
            kCGImageSourceCreateThumbnailWithTransform: true,
            kCGImageSourceThumbnailMaxPixelSize: decodeLimit,
        ]
        guard let image = CGImageSourceCreateThumbnailAtIndex(source, 0, options as CFDictionary) else {
            throw Failure.unreadable
        }
        return try png(from: image)
    }

    /// Scale `image` to fill the square, crop it to the center and encode
    /// it as an 8-bit RGBA PNG.
    static func png(from image: CGImage) throws -> Data {
        let width = CGFloat(image.width), height = CGFloat(image.height)
        guard width > 0, height > 0, let space = CGColorSpace(name: CGColorSpace.sRGB),
            let context = CGContext(
                data: nil, width: side, height: side, bitsPerComponent: 8, bytesPerRow: 0, space: space,
                bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue)
        else { throw Failure.unreadable }
        let scale = CGFloat(side) / min(width, height)
        let drawn = CGSize(width: width * scale, height: height * scale)
        context.interpolationQuality = .high
        context.draw(
            image,
            in: CGRect(
                x: (CGFloat(side) - drawn.width) / 2, y: (CGFloat(side) - drawn.height) / 2,
                width: drawn.width, height: drawn.height))
        guard let square = context.makeImage() else { throw Failure.unreadable }
        let data = NSMutableData()
        guard let destination = CGImageDestinationCreateWithData(data, UTType.png.identifier as CFString, 1, nil)
        else { throw Failure.unreadable }
        CGImageDestinationAddImage(destination, square, nil)
        guard CGImageDestinationFinalize(destination) else { throw Failure.unreadable }
        guard data.length <= maximumPngBytes else { throw Failure.tooComplex }
        return data as Data
    }
}

/// Decoded account logos, shared across surfaces (spec/08 "Logo").
@MainActor
enum AccountLogoImages {
    private static var cache: [String: (revision: Int64?, png: Data, image: NSImage?)] = [:]

    /// The account's logo, or nil without one or when the stored PNG
    /// can't be decoded: the monogram shows instead.
    static func image(for account: AccountAppearance?) -> NSImage? {
        guard let account, let png = account.logoPng else { return nil }
        // A restored archive can reuse a revision for different pixels.
        if let cached = cache[account.accountId], cached.revision == account.logoRevision, cached.png == png {
            return cached.image
        }
        let image = NSImage(data: png).flatMap { $0.isValid ? $0 : nil }
        cache[account.accountId] = (account.logoRevision, png, image)
        return image
    }

    /// Whether the account stores a logo the shell can't decode; Settings
    /// notes it.
    static func isUndecodable(_ account: AccountAppearance) -> Bool {
        account.logoPng != nil && image(for: account) == nil
    }

    /// The logo an identity shows: its account's, unless the identity has
    /// its own symbol override, which wins.
    static func image(for identity: IdentityAppearance, in appearance: Appearance?) -> NSImage? {
        guard identity.symbolOverride == nil else { return nil }
        return image(for: appearance?.accounts.first { $0.accountId == identity.accountId })
    }

    /// The ring's width: 1 pt, or 2 pt with Increase Contrast or
    /// Differentiate Without Color.
    static func ringWidth(increaseContrast: Bool, differentiate: Bool) -> CGFloat {
        increaseContrast || differentiate ? 2 : 1
    }

    /// Draw a logo at a monogram's frame: clipped to a rounded square on an
    /// opaque plate of the window background and ringed in the marker
    /// color, so the account color shows around any logo.
    static func draw(_ image: NSImage, in rect: NSRect, ring: NSColor, ringWidth: CGFloat) {
        let radius = rect.width * 0.22
        let shape = NSBezierPath(roundedRect: rect, xRadius: radius, yRadius: radius)
        NSGraphicsContext.saveGraphicsState()
        shape.addClip()
        NSColor.windowBackgroundColor.setFill()
        rect.fill()
        image.draw(in: rect, from: .zero, operation: .sourceOver, fraction: 1)
        NSGraphicsContext.restoreGraphicsState()
        ring.setStroke()
        let inset = rect.insetBy(dx: ringWidth / 2, dy: ringWidth / 2)
        let ringShape = NSBezierPath(roundedRect: inset, xRadius: radius, yRadius: radius)
        ringShape.lineWidth = ringWidth
        ringShape.stroke()
    }
}

/// A logo in SwiftUI surfaces (account chips, Settings previews), drawn
/// like [`AccountLogoImages.draw`]: decorative, so it adds nothing to the
/// accessibility label.
struct AccountLogoMark: View {
    let image: NSImage
    let ring: Color
    let side: CGFloat
    @Environment(\.accessibilityDifferentiateWithoutColor) private var differentiate
    @Environment(\.colorSchemeContrast) private var contrast

    var body: some View {
        let shape = RoundedRectangle(cornerRadius: side * 0.22)
        Image(nsImage: image)
            .resizable()
            .interpolation(.high)
            .frame(width: side, height: side)
            .background(Color(nsColor: .windowBackgroundColor))
            .clipShape(shape)
            .overlay(
                shape.strokeBorder(
                    ring,
                    lineWidth: AccountLogoImages.ringWidth(
                        increaseContrast: contrast == .increased, differentiate: differentiate)))
            .accessibilityHidden(true)
    }
}
