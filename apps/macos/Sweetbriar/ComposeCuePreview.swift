import AppKit
import SweetbriarBridge

/// The top of a compose window in one mode, drawn with the real strip.
/// It is a picture: it takes neither clicks nor focus, and the mode's
/// radio button says in words what it shows.
@MainActor
final class ComposeCuePreview: NSView {
    private static let width: CGFloat = 320
    private static let titlebarHeight: CGFloat = 16
    /// The top of the editor under the strip, where Color Wash shows.
    private static let editorHeight: CGFloat = 20

    let mode: ComposeCueMode
    let strip = IdentityStripView(
        frame: NSRect(x: 0, y: 0, width: ComposeCuePreview.width, height: 32))

    var content: IdentityStripView.Content {
        get { strip.content }
        set {
            strip.content = newValue
            needsDisplay = true
        }
    }
    var windowTint: NSColor? {
        ComposeCuePresentation.windowTint(
            mode: mode, content: strip.content, appearance: effectiveAppearance)
    }
    /// Color Wash's stops under the strip; nil shows the editor background.
    var wash: ComposeWashView.Stops? {
        ComposeCuePresentation.wash(mode: mode, content: strip.content, appearance: effectiveAppearance)
    }

    override var isFlipped: Bool { true }

    init(mode: ComposeCueMode) {
        self.mode = mode
        super.init(
            frame: NSRect(
                x: 0, y: 0, width: Self.width, height: Self.titlebarHeight + 32 + Self.editorHeight))
        translatesAutoresizingMaskIntoConstraints = false
        setAccessibilityElement(false)
        setAccessibilityIdentifier("composeCuePreview-\(mode.rawValue)")
        strip.cueMode = mode
        strip.textFont = .preferredFont(forTextStyle: .footnote, options: [:])
        strip.setAccessibilityElement(false)
        strip.setAccessibilityIdentifier("")
        strip.translatesAutoresizingMaskIntoConstraints = false
        strip.setContentHuggingPriority(.required, for: .vertical)
        addSubview(strip)
        NSLayoutConstraint.activate([
            widthAnchor.constraint(equalToConstant: Self.width),
            strip.topAnchor.constraint(equalTo: topAnchor, constant: Self.titlebarHeight),
            strip.leadingAnchor.constraint(equalTo: leadingAnchor),
            strip.trailingAnchor.constraint(equalTo: trailingAnchor),
            strip.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -Self.editorHeight),
        ])
    }

    required init?(coder: NSCoder) { fatalError("ComposeCuePreview is built in code") }

    override func hitTest(_ point: NSPoint) -> NSView? { nil }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        effectiveAppearance.performAsCurrentDrawingAppearance {
            (windowTint ?? .windowBackgroundColor).setFill()
            NSRect(x: 0, y: 0, width: bounds.width, height: Self.titlebarHeight).fill()
            let editor = NSRect(
                x: 0, y: bounds.height - Self.editorHeight, width: bounds.width, height: Self.editorHeight)
            if let wash {
                // Flipped: 90° runs from the top edge down.
                wash.gradient?.draw(in: editor, angle: 90)
            } else {
                NSColor.textBackgroundColor.setFill()
                editor.fill()
            }
        }
    }
}
