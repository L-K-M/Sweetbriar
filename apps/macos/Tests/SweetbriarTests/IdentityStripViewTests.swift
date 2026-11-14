import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@Suite("Identity strip primitive") @MainActor
struct IdentityStripViewTests {
    private let label = "Work · Alex Example <alex@company.example>"

    private func makeView(width: CGFloat = 600) -> IdentityStripView {
        let view = IdentityStripView(frame: NSRect(x: 0, y: 0, width: width, height: 32))
        view.contrastOptions = { (false, false) }
        view.content = .identity(label: label, monogram: "W", tokens: sampleAccount.tokens)
        view.setFrameSize(NSSize(width: width, height: view.intrinsicContentSize.height))
        return view
    }

    @Test func oneAccessibleElementContainsTheFullIdentity() {
        let view = makeView()
        #expect(view.isAccessibilityElement())
        #expect(view.accessibilityRole() == .staticText)
        #expect(view.accessibilityLabel() == "W · \(label)")
        #expect(view.accessibilityChildren()?.isEmpty != false)
        view.content = .unavailable(label: "Sending identity unavailable")
        #expect(view.accessibilityLabel() == "Sending identity unavailable")
    }

    @Test func displayOptionNotificationsInvalidateExistingViews() async throws {
        let view = makeView()
        let window = NSWindow(
            contentRect: view.bounds, styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView?.addSubview(view)
        defer { window.close() }
        view.needsDisplay = true
        #expect(view.needsDisplay, "the fixture must support invalidation")
        view.needsDisplay = false
        NSWorkspace.shared.notificationCenter.post(
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil)
        for _ in 0..<100 where !view.needsDisplay {
            try await Task.sleep(for: .milliseconds(5))
        }
        #expect(view.needsDisplay)
    }

    @Test func narrowAndLargeTextGrowWithoutLosingTheAddress() {
        let view = makeView(width: 600)
        let wide = view.intrinsicContentSize.height
        #expect(wide >= 32)
        view.setFrameSize(NSSize(width: 120, height: wide))
        let narrow = view.intrinsicContentSize.height
        #expect(narrow > wide)
        #expect(view.accessibilityLabel()?.contains("alex@company.example") == true)
        view.textFont = .systemFont(ofSize: 26)
        let large = view.intrinsicContentSize.height
        #expect(large > narrow)
        view.setFrameSize(NSSize(width: 600, height: large))
        #expect(view.intrinsicContentSize.height < large)
    }

    @Test func longAddressDrawsAcrossMultipleLinesAtNarrowWidth() throws {
        let view = makeView(width: 120)
        view.appearance = NSAppearance(named: .aqua)
        let bitmap = try render(view)
        var rows: [Int] = []
        for y in 0..<bitmap.pixelsHigh {
            var ink = false
            for x in 48..<bitmap.pixelsWide {
                if let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.sRGB),
                    color.redComponent < 0.2 && color.greenComponent < 0.2 && color.blueComponent < 0.2
                {
                    ink = true
                    break
                }
            }
            if ink { rows.append(y) }
        }
        let first = try #require(rows.first)
        let last = try #require(rows.last)
        #expect(last - first > Int(view.textFont.pointSize * 2))
    }

    @Test func aLongStoredSymbolWrapsInsteadOfBeingClipped() throws {
        let view = makeView(width: 150)
        let initialHeight = view.intrinsicContentSize.height
        let symbol = String(repeating: "M", count: 30)
        view.content = .identity(label: label, monogram: symbol, tokens: sampleAccount.tokens)
        let height = view.intrinsicContentSize.height
        #expect(height > initialHeight)
        #expect(view.accessibilityLabel()?.contains(symbol) == true)
        view.setFrameSize(NSSize(width: 150, height: height))
        view.appearance = NSAppearance(named: .aqua)
        let bitmap = try render(view)
        var rows: [Int] = []
        for y in 0..<bitmap.pixelsHigh {
            for x in 8..<48 {
                guard let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else { continue }
                if color.blueComponent > color.redComponent + 0.1 {
                    rows.append(y)
                    break
                }
            }
        }
        let first = try #require(rows.first)
        let last = try #require(rows.last)
        #expect(last - first > Int(view.textFont.pointSize * 2))
    }

    @Test(arguments: [
        NSAppearance.Name.aqua, .darkAqua, .accessibilityHighContrastAqua, .accessibilityHighContrastDarkAqua,
    ])
    func actualStripFillUsesTheResolvedLightOrDarkToken(_ name: NSAppearance.Name) throws {
        let view = makeView()
        view.appearance = NSAppearance(named: name)
        let bitmap = try render(view)
        let pixel = try #require(
            bitmap.colorAt(x: bitmap.pixelsWide - 8, y: bitmap.pixelsHigh / 2)?.usingColorSpace(.sRGB))
        let dark = name == .darkAqua || name == .accessibilityHighContrastDarkAqua
        #expect(dark ? pixel.redComponent < 0.02 : pixel.redComponent > 0.98)
        #expect(pixel.alphaComponent > 0.99)
    }

    @Test func highContrastAndDifferentiateDrawTheCoreOutline() throws {
        for (name, increase, differentiate) in [
            (NSAppearance.Name.aqua, false, false),
            (.aqua, true, false),
            (.aqua, false, true),
            (.darkAqua, true, false),
            (.darkAqua, false, true),
            (.accessibilityHighContrastAqua, false, false),
            (.accessibilityHighContrastDarkAqua, false, false),
        ] {
            let view = makeView()
            var tokens = sampleAccount.tokens
            tokens.light.outlineSrgb = "#ff0000"
            tokens.dark.outlineSrgb = "#ff0000"
            view.content = .identity(label: label, monogram: "W", tokens: tokens)
            view.appearance = NSAppearance(named: name)
            view.contrastOptions = { (increase, differentiate) }
            let bitmap = try render(view)
            let red = countPixels(bitmap) {
                $0.redComponent > 0.8 && $0.greenComponent < 0.2 && $0.blueComponent < 0.2
            }
            // AppKit may resolve a requested high-contrast name to Aqua when
            // the system setting is off. The injected options exercise its
            // drawing path; an actual system-setting check remains separate.
            let resolved = view.appearance?.name
            let highContrast =
                resolved == .accessibilityHighContrastAqua
                || resolved == .accessibilityHighContrastDarkAqua
            #expect((red > 0) == (increase || differentiate || highContrast))
        }
    }

    @Test(arguments: [NSAppearance.Name.aqua, .darkAqua])
    func theActualLabelSurfaceUsesTheCoreForeground(_ name: NSAppearance.Name) throws {
        let view = makeView()
        view.appearance = NSAppearance(named: name)
        let bitmap = try render(view)
        var textPixels = 0
        for x in 50..<bitmap.pixelsWide {
            for y in 0..<bitmap.pixelsHigh {
                guard let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else { continue }
                if name == .aqua ? color.redComponent < 0.1 : color.redComponent > 0.9 {
                    textPixels += 1
                }
            }
        }
        #expect(textPixels > 0)
    }

    @Test(arguments: ["W", "❤️"])
    func monogramIsCompositedUsingTheCoreMarkerEvenForColorGlyphs(_ mark: String) throws {
        let view = makeView()
        view.content = .identity(label: label, monogram: mark, tokens: sampleAccount.tokens)
        view.appearance = NSAppearance(named: .aqua)
        let bitmap = try render(view)
        var ink = 0
        var nonMarker = 0
        for x in 8..<32 {
            for y in 0..<bitmap.pixelsHigh {
                guard let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else { continue }
                if color.redComponent < 0.95 || color.greenComponent < 0.95 || color.blueComponent < 0.95 {
                    ink += 1
                    if color.redComponent > color.greenComponent + 0.03
                        || color.greenComponent > color.blueComponent + 0.03
                    {
                        nonMarker += 1
                    }
                }
            }
        }
        #expect(ink > 0)
        #expect(nonMarker == 0)
    }

    private func render(_ view: NSView) throws -> NSBitmapImageRep {
        let bitmap = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: bitmap)
        return bitmap
    }

    private func countPixels(_ bitmap: NSBitmapImageRep, matching: (NSColor) -> Bool) -> Int {
        var count = 0
        for x in 0..<bitmap.pixelsWide {
            for y in 0..<bitmap.pixelsHigh {
                if let color = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.sRGB), matching(color) {
                    count += 1
                }
            }
        }
        return count
    }
}
