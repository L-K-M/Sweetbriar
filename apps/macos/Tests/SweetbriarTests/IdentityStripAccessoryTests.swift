import AppKit
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@Suite("Identity strip accessory") @MainActor
struct IdentityStripAccessoryTests {
    @Test func measuresWrappedContentAndTracksFullScreenHeight() throws {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 600, height: 400),
            styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        let accessory = IdentityStripAccessory(nibName: nil, bundle: nil)
        window.addTitlebarAccessoryViewController(accessory)
        #expect(accessory.layoutAttribute == .bottom)
        accessory.setContent(
            .identity(
                label: "Work · Alex Example <alex@company.example>", monogram: "W",
                tokens: sampleAccount.tokens))
        accessory.view.setFrameSize(NSSize(width: 600, height: accessory.view.frame.height))
        let wide = accessory.view.frame.height
        #expect(wide >= 32)
        #expect(accessory.fullScreenMinHeight == wide)
        #expect(accessory.strip.frame.size == accessory.view.bounds.size)
        accessory.view.setFrameSize(NSSize(width: 120, height: wide))
        let narrow = accessory.view.frame.height
        #expect(narrow > wide)
        #expect(accessory.fullScreenMinHeight == narrow)
        #expect(accessory.strip.accessibilityLabel()?.contains("alex@company.example") == true)
        accessory.setTextFont(.systemFont(ofSize: 26))
        #expect(accessory.view.frame.height > narrow)
        #expect(accessory.fullScreenMinHeight == accessory.view.frame.height)
        let stable = accessory.view.frame.size
        accessory.setTextFont(.systemFont(ofSize: 26))
        #expect(accessory.view.frame.size == stable)
        window.removeTitlebarAccessoryViewController(at: 0)
    }

    @Test func windowResizeRemeasuresTheInstalledAccessory() async throws {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 680, height: 400),
            styleMask: [.titled, .closable, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        let accessory = IdentityStripAccessory(nibName: nil, bundle: nil)
        window.addTitlebarAccessoryViewController(accessory)
        accessory.setContent(.unavailable(label: String(repeating: "long-address@example.test ", count: 5)))
        window.makeKeyAndOrderFront(nil)
        try await Task.sleep(for: .milliseconds(100))
        let wideWidth = accessory.view.bounds.width
        let wideHeight = accessory.view.frame.height
        window.setContentSize(NSSize(width: 400, height: 400))
        try await Task.sleep(for: .milliseconds(100))
        #expect(accessory.view.bounds.width < wideWidth)
        #expect(accessory.view.frame.height > wideHeight)
        #expect(accessory.view.frame.height == accessory.strip.intrinsicContentSize.height)
        #expect(accessory.fullScreenMinHeight == accessory.view.frame.height)
        window.removeTitlebarAccessoryViewController(at: 0)
    }

    @Test func updatesHeightWhenContentChanges() {
        let accessory = IdentityStripAccessory(nibName: nil, bundle: nil)
        accessory.loadViewIfNeeded()
        accessory.view.setFrameSize(NSSize(width: 120, height: 32))
        accessory.setContent(.unavailable(label: "None"))
        let short = accessory.view.frame.height
        accessory.setContent(.unavailable(label: String(repeating: "long address ", count: 20)))
        #expect(accessory.view.frame.height > short)
        #expect(accessory.fullScreenMinHeight == accessory.view.frame.height)
        accessory.setContent(.unavailable(label: "None"))
        #expect(accessory.view.frame.height == short)
        #expect(accessory.fullScreenMinHeight == short)
    }
}
