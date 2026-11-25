import AppKit
import SweetbriarBridge

/// Draws one resolved identity, without owning or deriving account appearance.
/// The containing titlebar accessory owns the view's width and full-screen height.
@MainActor
final class IdentityStripView: NSView {
    enum Content: Equatable {
        case identity(label: String, monogram: String, tokens: AppearanceTokens)
        case unavailable(label: String)

        // Swift's canonical String equality would keep an old accessibility
        // label when the displayed identity changes only in source spelling.
        static func == (lhs: Self, rhs: Self) -> Bool {
            switch (lhs, rhs) {
            case (.identity(let a, let markA, let tokensA), .identity(let b, let markB, let tokensB)):
                a.utf8.elementsEqual(b.utf8) && markA.utf8.elementsEqual(markB.utf8) && tokensA == tokensB
            case (.unavailable(let a), .unavailable(let b)):
                a.utf8.elementsEqual(b.utf8)
            default: false
            }
        }

        var label: String {
            switch self {
            case .identity(let label, _, _), .unavailable(let label): label
            }
        }
        var accessibilityLabel: String {
            switch self {
            case .identity(let label, let monogram, _):
                monogram.isEmpty ? label : "\(monogram) · \(label)"
            case .unavailable(let label): label
            }
        }
    }

    var content: Content = .unavailable(label: "Sending identity unavailable") {
        didSet {
            guard content != oldValue else { return }
            setAccessibilityLabel(content.accessibilityLabel)
            invalidateIntrinsicContentSize()
            needsDisplay = true
        }
    }
    /// The account color's name, exposed as the accessibility value so a
    /// UI test can observe a route's color (spec/08 "Identity strip and
    /// account colors"); empty for an unavailable route. The drawn
    /// tokens stay the source of truth for the fill.
    var colorName: String = "" {
        didSet {
            guard colorName != oldValue else { return }
            setAccessibilityValue(colorName)
        }
    }
    var textFont: NSFont = .preferredFont(forTextStyle: .body, options: [:]) {
        didSet {
            guard textFont != oldValue else { return }
            invalidateIntrinsicContentSize()
            needsDisplay = true
        }
    }
    /// Minimal keeps the fill and text neutral. An unread preference draws
    /// the strip's colors; the window says that it is still loading.
    var cueMode: ComposeCueMode? {
        didSet {
            guard cueMode != oldValue else { return }
            needsDisplay = true
        }
    }
    var appearanceDidChange: (() -> Void)?
    /// The account's logo, drawn in the monogram's place (spec/08
    /// "Logo"); nil draws the monogram.
    var logo: NSImage? {
        didSet {
            guard logo !== oldValue else { return }
            if (logo == nil) != (oldValue == nil) { invalidateIntrinsicContentSize() }
            needsDisplay = true
        }
    }
    var contrastOptions: () -> (increase: Bool, differentiate: Bool) = {
        (
            NSWorkspace.shared.accessibilityDisplayShouldIncreaseContrast,
            NSWorkspace.shared.accessibilityDisplayShouldDifferentiateWithoutColor
        )
    }

    private let padding: CGFloat = 8
    private let minimumHeight: CGFloat = 32
    private let fallbackWidth: CGFloat = 600
    override var isFlipped: Bool { true }
    override var isOpaque: Bool { true }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        setAccessibilityElement(true)
        setAccessibilityRole(.staticText)
        setAccessibilityIdentifier("composeIdentityStrip")
        setAccessibilityLabel(content.accessibilityLabel)
        NSWorkspace.shared.notificationCenter.addObserver(
            self, selector: #selector(displayOptionsChanged),
            name: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil)
    }

    @objc private nonisolated func displayOptionsChanged() {
        Task { @MainActor [weak self] in
            self?.invalidateIntrinsicContentSize()
            self?.needsDisplay = true
        }
    }

    required init?(coder: NSCoder) { fatalError("IdentityStripView is built in code") }

    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: height(for: bounds.width))
    }

    override func setFrameSize(_ newSize: NSSize) {
        let previousHeight = height(for: bounds.width)
        super.setFrameSize(newSize)
        if previousHeight != height(for: bounds.width) { invalidateIntrinsicContentSize() }
    }

    override func viewDidChangeEffectiveAppearance() {
        super.viewDidChangeEffectiveAppearance()
        needsDisplay = true
        appearanceDidChange?()
    }

    override func draw(_ dirtyRect: NSRect) {
        super.draw(dirtyRect)
        effectiveAppearance.performAsCurrentDrawingAppearance {
            switch content {
            case .identity(let label, let monogram, let tokens):
                let dark = effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
                let variant = dark ? tokens.dark : tokens.light
                let minimal = cueMode == .minimal
                let surface = minimal ? NSColor.windowBackgroundColor : appearanceColor(variant.stripSrgb)
                let marker = appearanceColor(variant.markerSrgb)
                surface.setFill()
                bounds.fill()
                let hasMonogram = !monogram.isEmpty
                let text = minimal ? NSColor.labelColor : appearanceColor(variant.foregroundSrgb)
                drawLabel(label, color: text)
                let name = appearance?.name ?? effectiveAppearance.name
                let options = contrastOptions()
                if let logo {
                    AccountLogoImages.draw(
                        logo, in: logoRect(), ring: marker,
                        ringWidth: AccountLogoImages.ringWidth(
                            increaseContrast: options.increase, differentiate: options.differentiate))
                } else if hasMonogram {
                    drawMonogram(monogram, color: marker)
                }
                let faintMarker =
                    minimal && ComposeCuePresentation.markerNeedsOutline(marker, on: surface)
                if hasMonogram && logo == nil && (options.differentiate || faintMarker) {
                    // The outline token holds 3:1 against the strip; on
                    // Minimal's neutral surface the label color does.
                    let ring = minimal ? NSColor.labelColor : appearanceColor(variant.outlineSrgb)
                    ring.setStroke()
                    let markFrame = NSRect(
                        x: padding - 3, y: 3, width: markWidth(for: bounds.width) + 6,
                        height: max(0, bounds.height - 6))
                    let shape = NSBezierPath(roundedRect: markFrame, xRadius: 4, yRadius: 4)
                    shape.lineWidth = 1.5
                    shape.stroke()
                }
                if name == .accessibilityHighContrastAqua || name == .accessibilityHighContrastDarkAqua
                    || options.increase || options.differentiate
                {
                    let edge = minimal ? NSColor.labelColor : appearanceColor(variant.outlineSrgb)
                    edge.setStroke()
                    let outline = NSBezierPath(rect: bounds.insetBy(dx: 0.75, dy: 0.75))
                    outline.lineWidth = 1.5
                    outline.stroke()
                }
            case .unavailable(let label):
                NSColor.windowBackgroundColor.setFill()
                bounds.fill()
                drawLabel(label, color: .labelColor)
            }
        }
    }

    /// The logo's square, at the monogram's size, centered in the mark's
    /// column.
    func logoRect() -> NSRect {
        let side = min(max(24, ceil(textFont.pointSize * 1.7)), max(0, bounds.height - 8))
        let column = markWidth(for: bounds.width)
        return NSRect(
            x: padding + (column - side) / 2, y: (bounds.height - side) / 2, width: side, height: side)
    }

    private func markWidth(for width: CGFloat) -> CGFloat {
        guard case .identity(_, let monogram, _) = content else { return 0 }
        let minimum = max(24, ceil(textFont.pointSize * 1.7))
        if monogram.isEmpty { return logo == nil ? 0 : minimum }
        let natural = (monogram as NSString).size(withAttributes: markAttributes()).width
        // Stored symbols can contain more than one character. Wrap them rather
        // than clipping them or allowing them to consume the whole address.
        return max(minimum, min(ceil(natural), max(minimum, (width - padding * 3) / 3)))
    }
    private func labelInset(for width: CGFloat) -> CGFloat {
        let column = markWidth(for: width)
        return column > 0 ? padding * 2 + column : padding
    }
    private func height(for width: CGFloat) -> CGFloat {
        let width = width > 0 ? width : fallbackWidth
        let labelWidth = max(1, width - padding - labelInset(for: width))
        let label = NSAttributedString(string: content.label, attributes: labelAttributes(color: .labelColor))
        let rect = label.boundingRect(
            with: NSSize(
                width: labelWidth, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading])
        var markHeight: CGFloat = 0
        if case .identity(_, let monogram, _) = content, !monogram.isEmpty {
            let mark = NSAttributedString(string: monogram, attributes: markAttributes())
            markHeight =
                mark.boundingRect(
                    with: NSSize(width: markWidth(for: width), height: .greatestFiniteMagnitude),
                    options: [.usesLineFragmentOrigin, .usesFontLeading]
                ).height
        }
        return max(minimumHeight, ceil(max(rect.height, markHeight)) + padding * 2)
    }
    private func markAttributes() -> [NSAttributedString.Key: Any] {
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = .byCharWrapping
        paragraph.alignment = .center
        return [
            .font: NSFont.systemFont(
                ofSize: textFont.pointSize, weight: contrastOptions().differentiate ? .heavy : .bold),
            .foregroundColor: NSColor.white, .paragraphStyle: paragraph,
        ]
    }
    private func labelAttributes(color: NSColor) -> [NSAttributedString.Key: Any] {
        let paragraph = NSMutableParagraphStyle()
        paragraph.lineBreakMode = .byCharWrapping
        return [.font: textFont, .foregroundColor: color, .paragraphStyle: paragraph]
    }
    private func drawLabel(_ label: String, color: NSColor) {
        let x = labelInset(for: bounds.width)
        let text = NSAttributedString(string: label, attributes: labelAttributes(color: color))
        let width = max(1, bounds.width - padding - x)
        let measured = text.boundingRect(
            with: NSSize(width: width, height: .greatestFiniteMagnitude),
            options: [.usesLineFragmentOrigin, .usesFontLeading])
        text.draw(
            with: NSRect(
                x: x, y: max(padding, (bounds.height - ceil(measured.height)) / 2),
                width: width, height: ceil(measured.height)),
            options: [.usesLineFragmentOrigin, .usesFontLeading])
    }
    private func drawMonogram(_ monogram: String, color: NSColor) {
        guard let context = NSGraphicsContext.current?.cgContext else { return }
        let text = NSAttributedString(string: monogram, attributes: markAttributes())
        let width = markWidth(for: bounds.width)
        let height = ceil(
            text.boundingRect(
                with: NSSize(width: width, height: .greatestFiniteMagnitude),
                options: [.usesLineFragmentOrigin, .usesFontLeading]
            ).height)
        let rect = NSRect(x: padding, y: 0, width: width, height: bounds.height)
        // Color glyphs (including emoji) must use the core marker on this
        // surface too. Composite the text's alpha, not its original colors.
        context.saveGState()
        context.clip(to: rect)
        context.beginTransparencyLayer(auxiliaryInfo: nil)
        text.draw(
            with: NSRect(
                x: rect.minX, y: max(padding, (bounds.height - height) / 2),
                width: width, height: height),
            options: [.usesLineFragmentOrigin, .usesFontLeading])
        context.setBlendMode(.sourceIn)
        context.setFillColor(color.cgColor)
        context.fill(rect)
        context.endTransparencyLayer()
        context.restoreGState()
    }
}
