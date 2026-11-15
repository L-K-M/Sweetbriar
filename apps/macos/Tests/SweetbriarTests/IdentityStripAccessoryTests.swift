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

    @Test func displayOptionsRefreshThePreferredTextSize() async throws {
        let accessory = IdentityStripAccessory(nibName: nil, bundle: nil)
        accessory.loadViewIfNeeded()
        accessory.view.setFrameSize(NSSize(width: 120, height: 32))
        accessory.setContent(.unavailable(label: String(repeating: "long-address@example.test ", count: 3)))
        accessory.setTextFont(.systemFont(ofSize: 13))
        let initialHeight = accessory.view.frame.height
        accessory.preferredTextFont = { .systemFont(ofSize: 26) }
        NSWorkspace.shared.notificationCenter.post(
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil)
        for _ in 0..<100 where accessory.strip.textFont.pointSize != 26 {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(accessory.strip.textFont.pointSize == 26)
        #expect(accessory.view.frame.height > initialHeight)
        #expect(accessory.fullScreenMinHeight == accessory.view.frame.height)
        accessory.preferredTextFont = { .systemFont(ofSize: 13) }
        NotificationCenter.default.post(name: NSApplication.didBecomeActiveNotification, object: NSApp)
        for _ in 0..<100 where accessory.strip.textFont.pointSize != 13 {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(accessory.strip.textFont.pointSize == 13)
        #expect(accessory.view.frame.height == initialHeight)
        #expect(accessory.fullScreenMinHeight == initialHeight)
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
        for _ in 0..<100 where accessory.view.bounds.width <= 600 {
            try await Task.sleep(for: .milliseconds(10))
        }
        let wideWidth = accessory.view.bounds.width
        try #require(wideWidth > 600)
        let wideHeight = accessory.view.frame.height
        window.setContentSize(NSSize(width: 400, height: 400))
        for _ in 0..<100 where accessory.view.bounds.width >= wideWidth {
            try await Task.sleep(for: .milliseconds(10))
        }
        try #require(accessory.view.bounds.width < wideWidth)
        #expect(accessory.view.frame.height > wideHeight)
        #expect(accessory.view.frame.height == accessory.strip.intrinsicContentSize.height)
        #expect(accessory.fullScreenMinHeight == accessory.view.frame.height)
        window.removeTitlebarAccessoryViewController(at: 0)
    }

    @Test func onlyRouteChangesAnimateAndReduceMotionDisablesTheTransition() async throws {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 600, height: 400),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        let accessory = IdentityStripAccessory(nibName: nil, bundle: nil)
        window.addTitlebarAccessoryViewController(accessory)
        window.makeKeyAndOrderFront(nil)
        accessory.reduceMotion = { false }
        accessory.setContent(.unavailable(label: "First"))
        let oldHeight = accessory.strip.bounds.height
        accessory.setContent(
            .unavailable(label: String(repeating: "Long identity ", count: 25)), routeChanged: true)
        let snapshot = try #require(accessory.strip.subviews.first as? NSImageView)
        #expect(accessory.strip.bounds.height > oldHeight)
        #expect(snapshot.frame.size == accessory.strip.bounds.size)
        #expect(snapshot.image != nil)
        #expect(!snapshot.isAccessibilityElement())
        #expect(IdentityStripAccessory.transitionDuration == 0.15)
        for _ in 0..<100 where !accessory.strip.subviews.isEmpty {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(accessory.strip.subviews.isEmpty)
        accessory.setContent(.unavailable(label: "Next"), routeChanged: true)
        let earlier = try #require(accessory.strip.subviews.first)
        accessory.setContent(.unavailable(label: "Later"), routeChanged: true)
        #expect(earlier.superview == nil)
        #expect(accessory.strip.subviews.count == 1)
        accessory.setContent(.unavailable(label: "Appearance changed"))
        #expect(accessory.strip.subviews.isEmpty)
        accessory.reduceMotion = { true }
        accessory.setContent(.unavailable(label: "Third"), routeChanged: true)
        #expect(accessory.strip.subviews.isEmpty)
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
