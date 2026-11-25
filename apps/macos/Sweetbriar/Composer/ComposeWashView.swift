import AppKit

/// Color Wash's gradient below a compose window's header (spec/08
/// "Color Wash mode"): the wash's top stop at the top, its bottom stop
/// at the bottom. Without stops it draws nothing. It is a backdrop: it
/// takes no clicks and isn't an accessibility element.
@MainActor
final class ComposeWashView: NSView {
    struct Stops: Equatable {
        let top: NSColor
        let bottom: NSColor
    }

    var stops: Stops? {
        didSet {
            guard stops != oldValue else { return }
            needsDisplay = true
        }
    }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setAccessibilityElement(false)
    }

    required init?(coder: NSCoder) { fatalError("ComposeWashView is built in code") }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func draw(_ dirtyRect: NSRect) {
        guard let stops else { return }
        // In unflipped coordinates -90° runs from the top edge down.
        stops.gradient?.draw(in: bounds, angle: -90)
    }
}

extension ComposeWashView.Stops {
    /// Interpolated in sRGB, so every color lies channel by channel
    /// between the stops, which the core's contrast check relies on
    /// (spec/08 "The wash stops"). NSGradient's default space is generic
    /// RGB, whose blend leaves that range.
    var gradient: NSGradient? {
        NSGradient(colors: [top, bottom], atLocations: [0, 1], colorSpace: .sRGB)
    }
}
