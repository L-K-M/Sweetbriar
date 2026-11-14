import AppKit

/// Owns the strip's titlebar sizing. The window owns this accessory; it does
/// not own identity discovery or the appearance subscription.
@MainActor
final class IdentityStripAccessory: NSTitlebarAccessoryViewController {
    let strip = IdentityStripView(frame: NSRect(x: 0, y: 0, width: 600, height: 32))
    private var measuring = false

    override init(nibName nibNameOrNil: NSNib.Name?, bundle nibBundleOrNil: Bundle?) {
        super.init(nibName: nibNameOrNil, bundle: nibBundleOrNil)
        layoutAttribute = .bottom
        // The standard titlebar height would clip a wrapped identity label.
        automaticallyAdjustsSize = false
        NSWorkspace.shared.notificationCenter.addObserver(
            self, selector: #selector(displayOptionsChanged),
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil)
    }

    required init?(coder: NSCoder) { fatalError("IdentityStripAccessory is built in code") }

    override func loadView() {
        let host = IdentityStripHost(frame: NSRect(x: 0, y: 0, width: 600, height: 32))
        host.resized = { [weak self] in self?.measure() }
        host.addSubview(strip)
        view = host
        measure()
    }

    func setContent(_ content: IdentityStripView.Content) {
        strip.content = content
        measure()
    }

    func setTextFont(_ font: NSFont) {
        strip.textFont = font
        measure()
    }

    @objc private nonisolated func displayOptionsChanged() {
        Task { @MainActor [weak self] in self?.measure() }
    }

    private func measure() {
        guard isViewLoaded, !measuring else { return }
        measuring = true
        defer { measuring = false }
        let width = max(1, view.bounds.width)
        strip.setFrameSize(NSSize(width: width, height: strip.frame.height))
        let height = strip.intrinsicContentSize.height
        if view.frame.height != height {
            view.setFrameSize(NSSize(width: view.frame.width, height: height))
        }
        if strip.frame.size != NSSize(width: width, height: height) {
            strip.setFrameSize(NSSize(width: width, height: height))
        }
        if fullScreenMinHeight != height { fullScreenMinHeight = height }
    }
}

@MainActor
private final class IdentityStripHost: NSView {
    var resized: (() -> Void)?

    override func setFrameSize(_ newSize: NSSize) {
        let oldSize = frame.size
        super.setFrameSize(newSize)
        if oldSize != newSize { resized?() }
    }
}
