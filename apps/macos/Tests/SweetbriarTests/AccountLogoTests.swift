// Account logos in the shell (spec/08 "Logo"; Settings → Appearance
// "Logo"): the pipeline, Settings' writes, and the logo in place of the
// monogram in the strip, the From menu's swatches, the sidebar, chips
// and previews, with the undecodable and identity-override cases.

import AppKit
import ImageIO
import SwiftUI
import Testing
import UniformTypeIdentifiers
import SweetbriarBridge
@testable import Sweetbriar

private let logoTokens = AppearanceTokens(
    light: .init(
        markerSrgb: "#0072b2", stripSrgb: "#eaf3ff", foregroundSrgb: "#000000", outlineSrgb: "#000000",
        washTopSrgb: "#daeeff", washBottomSrgb: "#f6fbff", markerInkSrgb: "#ffffff"),
    dark: .init(
        markerSrgb: "#56b4e9", stripSrgb: "#1c2530", foregroundSrgb: "#ffffff", outlineSrgb: "#ffffff",
        washTopSrgb: "#142b3d", washBottomSrgb: "#151b21", markerInkSrgb: "#000000"))

/// A `width` x `height` sRGB image whose pixels `fill` decides.
private func image(width: Int, height: Int, fill: (Int, Int) -> (UInt8, UInt8, UInt8)) -> CGImage {
    var bytes = [UInt8](repeating: 255, count: width * height * 4)
    for y in 0..<height {
        for x in 0..<width {
            let (r, g, b) = fill(x, y)
            let i = (y * width + x) * 4
            bytes[i] = r; bytes[i + 1] = g; bytes[i + 2] = b
        }
    }
    let provider = CGDataProvider(data: Data(bytes) as CFData)!
    return CGImage(
        width: width, height: height, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: width * 4,
        space: CGColorSpace(name: CGColorSpace.sRGB)!,
        bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.noneSkipLast.rawValue), provider: provider,
        decode: nil, shouldInterpolate: false, intent: .defaultIntent)!
}

private func pngFile(_ image: CGImage) throws -> URL {
    let url = FileManager.default.temporaryDirectory.appendingPathComponent("\(UUID().uuidString).png")
    let destination = try #require(CGImageDestinationCreateWithURL(url as CFURL, UTType.png.identifier as CFString, 1, nil))
    CGImageDestinationAddImage(destination, image, nil)
    try #require(CGImageDestinationFinalize(destination))
    return url
}

/// A solid red logo, as the core stores it.
private func redLogo() throws -> Data {
    try AccountLogoPipeline.png(from: image(width: 128, height: 128) { _, _ in (255, 0, 0) })
}

private func account(logo: Data?, revision: Int64? = 1, id: String = "work") -> AccountAppearance {
    AccountAppearance(
        accountId: id, colorSrgb: "#0072b2", tokens: logoTokens, symbol: nil, monogram: "W", collides: false,
        assignment: .paletteV1(slot: 0), revision: 3, logoPng: logo, logoRevision: logo == nil ? nil : revision)
}

private func identity(symbolOverride: String?) -> IdentityAppearance {
    IdentityAppearance(
        identityId: "work-1", accountId: "work", colorSrgb: "#0072b2", colorOverrideSrgb: nil, tokens: logoTokens,
        symbolOverride: symbolOverride, monogram: symbolOverride ?? "W", revision: nil)
}

/// One pixel of `view` drawn in light mode, from raw sRGB bytes.
@MainActor
private func pixel(of view: NSView, at point: NSPoint) throws -> (r: Int, g: Int, b: Int) {
    view.appearance = NSAppearance(named: .aqua)
    let width = Int(view.bounds.width), height = Int(view.bounds.height)
    let bitmap = try #require(
        NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8, samplesPerPixel: 4,
            hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0
        )?.retagging(with: .sRGB))
    bitmap.size = view.bounds.size
    view.cacheDisplay(in: view.bounds, to: bitmap)
    var bytes = [Int](repeating: 0, count: 4)
    let row = view.isFlipped ? Int(point.y) : height - 1 - Int(point.y)
    bitmap.getPixel(&bytes, atX: Int(point.x), y: row)
    return (bytes[0], bytes[1], bytes[2])
}

private func isRed(_ pixel: (r: Int, g: Int, b: Int)) -> Bool { pixel.r > 230 && pixel.g < 30 && pixel.b < 30 }

/// Records logo writes; the next one may fail with `Conflict`.
private actor LogoWrites: AccountLogoSource {
    struct Call: Equatable { let accountId: String; let revision: Int64?; let png: Data? }
    private(set) var calls: [Call] = []
    var conflict = false
    func failNext() { conflict = true }
    func setAccountLogo(accountId: String, expectedRevision: Int64?, png: Data?) async throws -> Receipt {
        calls.append(.init(accountId: accountId, revision: expectedRevision, png: png))
        if conflict {
            conflict = false
            throw CoreFailure.conflict(detail: "stale")
        }
        return .init(command: "set_account_logo", resultJson: "{}", createdAtMs: 0)
    }
}

/// The appearance snapshot `account` answers.
private actor OneAccountSource: AppearanceSource {
    let account: AccountAppearance
    init(_ account: AccountAppearance) { self.account = account }
    func appearance() async throws -> Appearance { Appearance(generation: 1, accounts: [account], identities: []) }
    func appearanceEvents(generation: UInt64) async throws -> any AppearanceEventSource {
        throw CoreFailure.unavailable(reason: "Not used by this fixture.")
    }
    func setAccountAppearance(
        accountId: String, expectedRevision: Int64, change: AccountAppearanceChange
    ) async throws -> Receipt { throw CoreFailure.unavailable(reason: "Not used by this fixture.") }
    func setIdentityAppearance(
        identityId: String, expectedRevision: Int64?, change: IdentityAppearanceChange
    ) async throws -> Receipt { throw CoreFailure.unavailable(reason: "Not used by this fixture.") }
    nonisolated func palette() -> [PaletteColor] { [] }
}

private struct LogoClock: FrameClock {
    func nextFrame() async { await Task.yield() }
}

@MainActor
struct AccountLogoTests {
    // MARK: The pipeline

    /// The size check comes before decoding: a file of 20 MB and one
    /// byte is refused as too large whatever it holds.
    @Test func aFileOver20MBIsRefusedBeforeDecoding() throws {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("\(UUID().uuidString).png")
        try #require(FileManager.default.createFile(atPath: url.path, contents: nil))
        let handle = try FileHandle(forWritingTo: url)
        try handle.truncate(atOffset: UInt64(AccountLogoPipeline.maximumFileBytes + 1))
        try handle.close()
        defer { try? FileManager.default.removeItem(at: url) }
        #expect(throws: AccountLogoPipeline.Failure.fileTooLarge) { try AccountLogoPipeline.png(fromFileAt: url) }
        #expect(AccountLogoPipeline.Failure.fileTooLarge.errorDescription == "Choose an image of 20 MB or less.")
    }

    /// A wide image is scaled to fill and cropped to its center: only the
    /// middle third of a red, green, blue image remains, as a 128 x 128
    /// 8-bit RGBA PNG within 65,536 bytes.
    @Test func anImageBecomesA128PixelPngCroppedToItsCenter() throws {
        let url = try pngFile(
            image(width: 300, height: 100) { x, _ in x < 100 ? (255, 0, 0) : x < 200 ? (0, 255, 0) : (0, 0, 255) })
        defer { try? FileManager.default.removeItem(at: url) }
        let png = try AccountLogoPipeline.png(fromFileAt: url)
        #expect(png.count <= AccountLogoPipeline.maximumPngBytes)
        let bytes = [UInt8](png)
        #expect(bytes.prefix(8) == [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])
        // IHDR: width and height, then bit depth 8 and color type 6 (RGBA).
        #expect(Array(bytes[12..<16]) == Array("IHDR".utf8))
        #expect(Array(bytes[16..<24]) == [0, 0, 0, 128, 0, 0, 0, 128])
        #expect(bytes[24] == 8 && bytes[25] == 6)
        #expect(Array(bytes.suffix(8).prefix(4)) == Array("IEND".utf8))
        let decoded = try #require(NSBitmapImageRep(data: png))
        for (x, y) in [(2, 2), (64, 64), (125, 125), (2, 125)] {
            let color = try #require(decoded.colorAt(x: x, y: y)?.usingColorSpace(.sRGB))
            #expect(color.greenComponent > 0.9 && color.redComponent < 0.1 && color.blueComponent < 0.1)
        }
    }

    /// Noise in every channel at the logo's own size doesn't compress
    /// below the bound: "Choose a simpler image."
    @Test func aNoiseImageIsRefusedAsTooComplex() throws {
        let side = AccountLogoPipeline.side
        let bytes = (0..<(side * side * 4)).map { _ in UInt8.random(in: 0...255) }
        let noise = try #require(
            CGImage(
                width: side, height: side, bitsPerComponent: 8, bitsPerPixel: 32, bytesPerRow: side * 4,
                space: CGColorSpace(name: CGColorSpace.sRGB)!,
                bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.last.rawValue),
                provider: CGDataProvider(data: Data(bytes) as CFData)!, decode: nil, shouldInterpolate: false,
                intent: .defaultIntent))
        #expect(throws: AccountLogoPipeline.Failure.tooComplex) { try AccountLogoPipeline.png(from: noise) }
        #expect(AccountLogoPipeline.Failure.tooComplex.errorDescription == "Choose a simpler image.")
    }

    // MARK: Settings

    /// Choose Image… writes the pipeline's PNG at the logo row's revision,
    /// Remove writes none, and a refused file writes nothing and says why.
    @Test func settingsWritesAndRemovesTheLogo() async throws {
        let stored = account(logo: try redLogo(), revision: 5)
        let appearance = AppearanceModel(source: OneAccountSource(stored))
        await appearance.refresh()
        let writes = LogoWrites()
        let model = AppearanceSettingsModel(appearance: appearance, logos: writes)
        let url = try pngFile(image(width: 64, height: 64) { _, _ in (0, 0, 255) })
        defer { try? FileManager.default.removeItem(at: url) }
        await model.chooseLogo(at: url, account: stored)
        let first = try #require(await writes.calls.first)
        #expect(first.accountId == "work" && first.revision == 5)
        #expect(first.png.map { $0.count <= AccountLogoPipeline.maximumPngBytes } == true)
        await model.setLogo(nil, account: stored)
        #expect(await writes.calls.last == .init(accountId: "work", revision: 5, png: nil))
        #expect(model.error == nil)

        let missing = FileManager.default.temporaryDirectory.appendingPathComponent("\(UUID().uuidString).png")
        await model.chooseLogo(at: missing, account: stored)
        #expect(await writes.calls.count == 2)
        #expect(model.error == "Sweetbriar can't read this image.")

        await writes.failNext()
        await model.setLogo(try redLogo(), account: stored)
        #expect(await writes.calls.count == 3)
        #expect(model.error?.contains("changed in another window") == true)
    }

    /// The real core accepts the pipeline's PNG, and `appearance()` maps
    /// it back with its revision; Remove clears it and keeps the row.
    @Test func theCoreStoresThePipelinesLogo() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent("sb-logo-\(UUID())")
        defer { try? FileManager.default.removeItem(at: root) }
        let core = try await CoreBridge.open(
            config: .init(
                dataRoot: root.appendingPathComponent("data").path,
                cacheRoot: root.appendingPathComponent("cache").path),
            snapshots: FrameBatcher<[Setting]>(clock: LogoClock()) { _ in })
        let appearance = AppearanceModel(source: core)
        await appearance.refresh()
        let archive = try #require(appearance.appearance?.accounts.first)
        #expect(archive.logoPng == nil && archive.logoRevision == nil)
        let model = AppearanceSettingsModel(appearance: appearance, logos: core)
        let url = try pngFile(image(width: 400, height: 300) { x, _ in x < 200 ? (200, 40, 40) : (40, 40, 200) })
        defer { try? FileManager.default.removeItem(at: url) }
        await model.chooseLogo(at: url, account: archive)
        #expect(model.error == nil)
        let stored = try #require(appearance.appearance?.accounts.first)
        #expect(stored.logoPng != nil && stored.logoRevision != nil)
        #expect(AccountLogoImages.image(for: stored) != nil)
        await model.setLogo(nil, account: stored)
        let cleared = try #require(appearance.appearance?.accounts.first)
        #expect(cleared.logoPng == nil && cleared.logoRevision != nil)
        #expect(model.error == nil)
        await core.shutdown()
    }

    // MARK: Surfaces

    /// The strip draws the logo in the monogram's place, ringed and on
    /// its plate; without a logo it draws the monogram.
    @Test func theStripDrawsTheLogoInTheMonogramsPlace() throws {
        let strip = IdentityStripView(frame: NSRect(x: 0, y: 0, width: 320, height: 40))
        strip.contrastOptions = { (false, false) }
        strip.content = .identity(label: "Work · <me@example.test>", monogram: "W", tokens: logoTokens)
        let logo = try #require(NSImage(data: try redLogo()))
        strip.logo = logo
        let rect = strip.logoRect()
        try #require(rect.width >= 16)
        #expect(isRed(try pixel(of: strip, at: NSPoint(x: rect.midX, y: rect.midY))))
        strip.logo = nil
        #expect(!isRed(try pixel(of: strip, at: NSPoint(x: rect.midX, y: rect.midY))))
    }

    @Test func theSidebarAndTheFromMenuSwatchDrawTheLogo() throws {
        let sidebar = SidebarMonogramView(frame: NSRect(x: 0, y: 0, width: 24, height: 24))
        sidebar.contrastOptions = { (false, false) }
        sidebar.account = account(logo: try redLogo())
        #expect(isRed(try pixel(of: sidebar, at: NSPoint(x: 12, y: 12))))

        let logo = try #require(NSImage(data: try redLogo()))
        let key = IdentitySwatch.Key(
            tokens: logoTokens, monogram: "W", appearance: NSAppearance(named: .aqua)!, logo: "work#1")
        let swatch = IdentitySwatch.image(key: key, logo: logo)
        let view = NSImageView(frame: NSRect(x: 0, y: 0, width: 18, height: 18))
        view.imageScaling = .scaleAxesIndependently
        view.image = swatch
        #expect(isRed(try pixel(of: view, at: NSPoint(x: 9, y: 9))))
    }

    /// Chips show the logo and keep their text label; an identity's own
    /// symbol wins over the account's logo.
    @Test func chipsShowTheLogoAndAnIdentitysSymbolWins() throws {
        let stored = account(logo: try redLogo())
        let snapshot = Appearance(generation: 1, accounts: [stored], identities: [identity(symbolOverride: nil)])
        let chip = AccountChipPresentation.make(
            label: "Received in Work", accountId: "work", appearance: snapshot, dark: false)
        #expect(chip.logo != nil)
        #expect(chip.label == "Received in Work")

        let inherited = AccountChipPresentation.make(
            label: "Me", identityId: "work-1", accountId: "work", appearance: snapshot, dark: false)
        #expect(inherited.logo != nil)
        let overridden = Appearance(
            generation: 1, accounts: [stored], identities: [identity(symbolOverride: "Q")])
        let own = AccountChipPresentation.make(
            label: "Me", identityId: "work-1", accountId: "work", appearance: overridden, dark: false)
        #expect(own.logo == nil && own.monogram == "Q")

        let host = NSHostingView(
            rootView: AccountChip(label: "Received in Work", accountId: "work", appearance: snapshot))
        host.layoutSubtreeIfNeeded()
        #expect(host.accessibilityChildren()?.count ?? 0 <= 1)
    }

    /// A stored logo the shell can't decode shows the monogram, and
    /// Settings notes it.
    @Test func anUndecodableLogoFallsBackToTheMonogram() {
        let broken = account(logo: Data("not a png".utf8), id: "broken")
        #expect(AccountLogoImages.image(for: broken) == nil)
        #expect(AccountLogoImages.isUndecodable(broken))
        let chip = AccountChipPresentation.make(
            label: "Broken", accountId: "broken",
            appearance: Appearance(generation: 1, accounts: [broken], identities: []), dark: false)
        #expect(chip.logo == nil && chip.monogram == "W")
        #expect(!AccountLogoImages.isUndecodable(account(logo: nil, id: "plain")))
    }

    /// A new logo revision decodes again; the same one comes from the cache.
    @Test func theCacheFollowsTheLogoRevision() throws {
        let first = account(logo: try redLogo(), revision: 1, id: "cached")
        let image = try #require(AccountLogoImages.image(for: first))
        #expect(AccountLogoImages.image(for: first) === image)
        let next = account(logo: try redLogo(), revision: 2, id: "cached")
        #expect(AccountLogoImages.image(for: next) !== image)
    }
}
