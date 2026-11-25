// A message window's tint (spec/08 Message window "Tint"): the
// receiving account's strip fill behind the title bar and the reader's
// header area in Color Wash and Strong, Increase Contrast's outline, Dark
// Mode, and a neutral window in Standard, Minimal and without the
// account's appearance.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import Foundation
import SweetbriarBridge
import Testing

private let tintTokens = AppearanceTokens(
    light: AppearanceVariant(
        markerSrgb: "#0072b2", stripSrgb: "#eaf3ff", foregroundSrgb: "#000000", outlineSrgb: "#1a1a1a",
        washTopSrgb: "#daeeff", washBottomSrgb: "#f6fbff", markerInkSrgb: "#ffffff"),
    dark: AppearanceVariant(
        markerSrgb: "#56b4e9", stripSrgb: "#1c2530", foregroundSrgb: "#ffffff", outlineSrgb: "#f0f0f0",
        washTopSrgb: "#142b3d", washBottomSrgb: "#151b21", markerInkSrgb: "#000000"))

/// One short plain-text message received in `account-a`.
private struct TintMessageSource: ReaderSource {
    let directory: URL

    init() throws {
        directory = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try Data("A synthetic body.".utf8).write(to: directory.appendingPathComponent("body.txt"))
    }

    func messageForOccurrence(occurrenceId: Int64) async throws -> Int64 { 7 }
    func getMessage(messageId: Int64) async throws -> ReaderMessage {
        ReaderMessage(
            messageId: 7,
            headers: .init(
                from: [.init(name: "Sender", address: "sender@example.test")],
                to: [.init(name: nil, address: "to@example.test")], cc: [], date: 1_792_238_400_000,
                subject: "Tinted", messageId: nil),
            accountId: "account-a", accountDisplayName: "Account A", occurrences: [],
            threadId: nil, tags: [], note: nil, bodyAvailability: .acquired)
    }
    func openBody(messageID: Int64, preferred: BodyKind) async throws -> BodyLease {
        BodyLease(
            leaseId: "tint", kind: .plainText,
            filePath: directory.appendingPathComponent("body.txt").path, byteLength: 0,
            remoteContentBlocked: 0, warnings: [], manifestPath: nil)
    }
    func openAttachment(messageID: Int64, partID: String) async throws -> AttachmentLease {
        throw CoreFailure.notFound(what: "attachment")
    }
    func releaseLease(_ leaseID: String) {}
}

/// The appearance snapshot, with or without `account-a`.
private actor TintAppearanceSource: AppearanceSource {
    let includesAccount: Bool
    init(includesAccount: Bool = true) { self.includesAccount = includesAccount }

    func appearance() async throws -> Appearance {
        let account = AccountAppearance(
            accountId: "account-a", colorSrgb: "#0072b2", tokens: tintTokens, symbol: nil, monogram: "A",
            collides: false, assignment: .paletteV1(slot: 0), revision: 1)
        return Appearance(generation: 1, accounts: includesAccount ? [account] : [], identities: [])
    }
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

private actor TintCueSource: ComposeCueSource {
    private var value: ComposeCueMode
    init(_ mode: ComposeCueMode) { value = mode }
    func composeCue() async throws -> ComposeCueMode { value }
    func setComposeCue(_ mode: ComposeCueMode) async throws -> Receipt {
        value = mode
        return .init(command: "set_setting", resultJson: "{}", createdAtMs: 1)
    }
}

@MainActor
private func tintEventually(_ check: () -> Bool) async -> Bool {
    for _ in 0..<500 {
        if check() { return true }
        try? await Task.sleep(for: .milliseconds(10))
    }
    return check()
}

private func near(_ color: NSColor?, _ hex: String) -> Bool {
    guard let color = color?.usingColorSpace(.sRGB), let want = appearanceColor(hex).usingColorSpace(.sRGB)
    else { return false }
    return abs(color.redComponent - want.redComponent) < 0.01
        && abs(color.greenComponent - want.greenComponent) < 0.01
        && abs(color.blueComponent - want.blueComponent) < 0.01
}

/// One rendered pixel of `view` at `point` (bottom-left origin), read as
/// raw sRGB bytes.
@MainActor
private func pixel(_ view: NSView, at point: NSPoint) throws -> NSColor {
    let width = Int(view.bounds.width.rounded()), height = Int(view.bounds.height.rounded())
    let bitmap = try #require(
        NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: width, pixelsHigh: height, bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
            bytesPerRow: 0, bitsPerPixel: 0)?.retagging(with: .sRGB))
    bitmap.size = view.bounds.size
    view.cacheDisplay(in: view.bounds, to: bitmap)
    var bytes = [Int](repeating: 0, count: 4)
    // Bitmap rows run from the top.
    bitmap.getPixel(&bytes, atX: Int(point.x), y: height - 1 - Int(point.y))
    return NSColor(
        srgbRed: CGFloat(bytes[0]) / 255, green: CGFloat(bytes[1]) / 255, blue: CGFloat(bytes[2]) / 255,
        alpha: 1)
}

/// A message window over `account-a`'s message, loaded and shown in
/// `appearanceName`; closed after `body`.
@MainActor
private func withMessageWindow(
    _ mode: ComposeCueMode?, includesAccount: Bool = true, appearanceName: NSAppearance.Name = .aqua,
    body: @MainActor (MessageWindowController, ComposeCueModel?) async throws -> Void
) async throws {
    let appearance = AppearanceModel(source: TintAppearanceSource(includesAccount: includesAccount))
    await appearance.refresh()
    var cue: ComposeCueModel?
    if let mode {
        let model = ComposeCueModel(source: TintCueSource(mode))
        await model.reload()
        cue = model
    }
    let source = try TintMessageSource()
    defer { try? FileManager.default.removeItem(at: source.directory) }
    let reader = ReaderViewController(
        model: ReaderModel(source: source, preferred: .plainText), appearance: appearance)
    let controller = MessageWindowController(
        occurrenceId: 11, reader: reader, markRead: nil, target: nil, appearance: appearance, cue: cue)
    controller.increaseContrast = { false }
    controller.window?.appearance = NSAppearance(named: appearanceName)
    controller.load()
    controller.showWindow(nil)
    defer { controller.close() }
    try #require(await tintEventually { controller.window?.title == "Tinted" })
    try await body(controller, cue)
}

@Suite("message window tint", .serialized, .timeLimit(.minutes(1)))
@MainActor
struct MessageWindowTintTests {
    /// Color Wash and Strong fill the title bar and the header area with
    /// the account's strip color and its labels with the foreground; the
    /// body keeps its surface.
    @Test(arguments: [ComposeCueMode.wash, .strong])
    func washAndStrongTintTheTitleBarAndHeader(_ mode: ComposeCueMode) async throws {
        try await withMessageWindow(mode) { controller, _ in
            try #require(await tintEventually { near(controller.window?.backgroundColor, "#eaf3ff") })
            let window = try #require(controller.window)
            #expect(window.titlebarAppearsTransparent)
            #expect(window.styleMask.contains(.fullSizeContentView))
            let reader = controller.reader
            #expect(near(reader.header.textColor, "#000000"))
            #expect(near(reader.remoteContent.textColor, "#000000"))
            #expect(controller.tint?.outline == nil)

            let backdrop = reader.backdrop
            backdrop.layoutSubtreeIfNeeded()
            let header = backdrop.headerRect
            try #require(!header.isEmpty)
            #expect(header.maxY == backdrop.bounds.maxY, "the header area runs under the title bar")
            // The 12-point margin beside the stack shows the backdrop.
            #expect(near(try pixel(backdrop, at: NSPoint(x: 4, y: header.midY)), "#eaf3ff"))
            let below = try pixel(backdrop, at: NSPoint(x: 4, y: max(1, header.minY - 20)))
            #expect(!near(below, "#eaf3ff"), "below the header the window background shows")
        }
    }

    /// Increase Contrast adds the outline; Dark Mode takes the dark
    /// variant.
    @Test func increaseContrastOutlinesAndDarkModeTakesTheDarkFill() async throws {
        try await withMessageWindow(.strong) { controller, _ in
            try #require(await tintEventually { controller.tint != nil })
            controller.increaseContrast = { true }
            #expect(near(controller.tint?.outline, "#1a1a1a"))
            controller.window?.appearance = NSAppearance(named: .darkAqua)
            #expect(await tintEventually { near(controller.window?.backgroundColor, "#1c2530") })
            #expect(near(controller.reader.header.textColor, "#ffffff"))
            #expect(near(controller.tint?.outline, "#f0f0f0"))
        }
    }

    /// Standard and Minimal leave the window neutral, as does a missing
    /// appearance or an unread mode; leaving Strong returns it to neutral.
    @Test func standardMinimalAndNoAppearanceStayNeutral() async throws {
        for mode in [ComposeCueMode.standard, .minimal] {
            try await withMessageWindow(mode) { controller, _ in
                try await Task.sleep(for: .milliseconds(50))
                Self.expectNeutral(controller)
            }
        }
        try await withMessageWindow(.strong, includesAccount: false) { controller, _ in
            try await Task.sleep(for: .milliseconds(50))
            Self.expectNeutral(controller)
        }
        try await withMessageWindow(nil) { controller, _ in
            try await Task.sleep(for: .milliseconds(50))
            Self.expectNeutral(controller)
        }
        try await withMessageWindow(.strong) { controller, cue in
            try #require(await tintEventually { controller.tint != nil })
            try #require(await cue?.setMode(.standard) == true)
            #expect(await tintEventually { controller.tint == nil })
            Self.expectNeutral(controller)
        }
    }

    private static func expectNeutral(_ controller: MessageWindowController) {
        #expect(controller.tint == nil)
        #expect(controller.window?.titlebarAppearsTransparent == false)
        #expect(controller.window?.styleMask.contains(.fullSizeContentView) == false)
        #expect(controller.reader.header.textColor == .labelColor)
        #expect(controller.reader.backdrop.tint == nil)
    }
}
#endif
