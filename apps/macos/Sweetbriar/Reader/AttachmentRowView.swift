import AppKit
import SweetbriarBridge
import UniformTypeIdentifiers

/// One reader attachment's presentation (spec/08 "Reader"): a compact
/// row with the file's type symbol, name and size. Pure, so the
/// app-hosted tests pin the media-type mapping without drawing.
struct AttachmentRowViewModel: Equatable {
    /// The attachment part the row opens.
    let partID: String
    /// The file's display name.
    let name: String
    /// The formatted size, or "damaged" when the manifest has none.
    let size: String
    /// The SF Symbol naming the file's type.
    let symbol: String
    /// The row's one accessibility label.
    let accessibilityLabel: String

    init(attachment: RenditionManifest.Attachment) {
        partID = attachment.partID
        name = attachment.displayName
        size = AttachmentRowViewModel.sizeText(attachment.bytes)
        symbol = Self.symbolName(for: attachment.mediaType)
        accessibilityLabel = "\(name), \(size)"
    }

    static func sizeText(_ bytes: UInt64?) -> String {
        guard let bytes else { return "damaged" }
        let formatter = ByteCountFormatter()
        formatter.countStyle = .file
        return formatter.string(fromByteCount: Int64(clamping: bytes))
    }

    /// The type symbol for a media type: an image, audio, video, PDF,
    /// text or spreadsheet gets its own mark; everything else is a
    /// document. Unknown media types stay legible rather than blank.
    static func symbolName(for mediaType: String) -> String {
        guard let type = UTType(mimeType: mediaType) else { return "doc.fill" }
        if type.conforms(to: .image) { return "photo" }
        if type.conforms(to: .audio) { return "music.note" }
        if type.conforms(to: .audiovisualContent) { return "film" }
        if type.conforms(to: .pdf) { return "doc.richtext" }
        let identifier = type.identifier.lowercased()
        if identifier.contains("spreadsheet") || identifier.contains("excel")
            || identifier.contains("comma-separated") || identifier.contains("tab-separated")
            || identifier.hasSuffix(".csv") {
            return "tablecells"
        }
        if type.conforms(to: .plainText) { return "doc.text" }
        return "doc.fill"
    }
}

/// A full-width attachment row: type symbol, truncating name, trailing
/// size and a hover wash. The whole row is one accessibility element
/// with a button role; clicking it opens the attachment.
@MainActor
final class AttachmentRowView: NSView {
    private let icon = NSImageView()
    private let name = NSTextField(labelWithString: "")
    private let size = NSTextField(labelWithString: "")
    private var tracking: NSTrackingArea?
    private var hovered = false
    /// The row's accessibility label, also the tests' readable value.
    private(set) var labelText = ""
    /// The attachment part this row opens.
    private(set) var attachmentPartID = ""
    /// Opens the attachment through the reader's lease path.
    var onOpen: (() -> Void)?

    init() {
        super.init(frame: .zero)
        icon.translatesAutoresizingMaskIntoConstraints = false
        icon.symbolConfiguration = NSImage.SymbolConfiguration(scale: .small)
        icon.contentTintColor = .secondaryLabelColor
        icon.setAccessibilityElement(false)
        addSubview(icon)

        name.translatesAutoresizingMaskIntoConstraints = false
        name.lineBreakMode = .byTruncatingTail
        name.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        name.setAccessibilityElement(false)
        addSubview(name)

        size.translatesAutoresizingMaskIntoConstraints = false
        size.font = .monospacedDigitSystemFont(ofSize: NSFont.smallSystemFontSize, weight: .regular)
        size.textColor = .secondaryLabelColor
        size.lineBreakMode = .byTruncatingTail
        size.setContentHuggingPriority(.required, for: .horizontal)
        size.setAccessibilityElement(false)
        addSubview(size)

        NSLayoutConstraint.activate([
            icon.leadingAnchor.constraint(equalTo: leadingAnchor, constant: 8),
            icon.centerYAnchor.constraint(equalTo: centerYAnchor),
            icon.widthAnchor.constraint(equalToConstant: 20),
            icon.heightAnchor.constraint(equalToConstant: 20),
            name.leadingAnchor.constraint(equalTo: icon.trailingAnchor, constant: 8),
            name.centerYAnchor.constraint(equalTo: centerYAnchor),
            size.leadingAnchor.constraint(greaterThanOrEqualTo: name.trailingAnchor, constant: 8),
            size.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -8),
            size.centerYAnchor.constraint(equalTo: centerYAnchor),
        ])

        setAccessibilityElement(true)
        setAccessibilityRole(.button)
    }

    required init?(coder: NSCoder) { fatalError("AttachmentRowView is built in code") }

    /// Fill the row from `model`; a new part id replaces the old target.
    func configure(_ model: AttachmentRowViewModel) {
        attachmentPartID = model.partID
        labelText = model.accessibilityLabel
        icon.image = NSImage(systemSymbolName: model.symbol, accessibilityDescription: nil)
        name.stringValue = model.name
        size.stringValue = model.size
        setAccessibilityLabel(model.accessibilityLabel)
    }

    /// The tests' click: the same path a mouse down takes.
    func performOpen() { onOpen?() }

    override var intrinsicContentSize: NSSize {
        NSSize(width: NSView.noIntrinsicMetric, height: 30)
    }

    override func mouseDown(with event: NSEvent) {
        onOpen?()
    }

    override func updateTrackingAreas() {
        super.updateTrackingAreas()
        if let tracking { removeTrackingArea(tracking) }
        tracking = NSTrackingArea(
            rect: bounds,
            options: [.mouseEnteredAndExited, .activeInKeyWindow],
            owner: self
        )
        if let tracking { addTrackingArea(tracking) }
    }

    override func mouseEntered(with event: NSEvent) {
        hovered = true
        needsDisplay = true
    }

    override func mouseExited(with event: NSEvent) {
        hovered = false
        needsDisplay = true
    }

    override func draw(_ dirtyRect: NSRect) {
        guard hovered else { return }
        NSColor.quaternaryLabelColor.withAlphaComponent(0.25).setFill()
        NSBezierPath(roundedRect: bounds.insetBy(dx: 2, dy: 1), xRadius: 6, yRadius: 6).fill()
    }
}
