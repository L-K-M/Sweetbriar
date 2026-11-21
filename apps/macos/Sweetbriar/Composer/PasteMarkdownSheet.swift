import AppKit
import SweetbriarBridge

/// Paste as Markdown's sheet (spec/08 "Paste"): a read-only preview of
/// the converted source, styled as the composer styles it, the lossy
/// warning, and Paste or Cancel. It holds the conversion captured when
/// it opened, so a later pasteboard change never alters it.
@MainActor
final class PasteMarkdownSheet: NSObject {
    enum State: Equatable {
        case converting
        case ready(MarkdownPaste)
        case refused(String)
    }

    /// One phrase per loss, in the core's order; nil when lossless.
    static func lossMessage(_ losses: [PasteLoss]) -> String? {
        guard !losses.isEmpty else { return nil }
        let phrases = losses.map { loss in
            switch loss {
            case .images: "images"
            case .styling: "text styles and colors"
            case .links: "some links"
            case .tables: "table layout"
            case .other: "other content"
            }
        }
        return "Some formatting can't be kept: \(phrases.joined(separator: ", "))."
    }

    let window: NSWindow
    let preview = ComposerTextView()
    let messageLabel = NSTextField(wrappingLabelWithString: "")
    let pasteButton = NSButton(title: "Paste", target: nil, action: nil)
    let cancelButton = NSButton(title: "Cancel", target: nil, action: nil)
    private(set) var state: State = .converting
    private var completion: (@MainActor (String?) -> Void)?

    override init() {
        window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 520, height: 360),
            styleMask: [.titled], backing: .buffered, defer: false)
        super.init()
        window.title = "Paste as Markdown"
        window.setAccessibilityIdentifier("pasteMarkdownSheet")

        preview.mode = .markdown
        preview.isEditable = false
        preview.isSelectable = true
        preview.setAccessibilityLabel("Markdown preview")
        preview.setAccessibilityIdentifier("pasteMarkdownPreview")
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 480, height: 240))
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder
        scroll.documentView = preview
        preview.minSize = NSSize(width: 0, height: scroll.contentSize.height)
        preview.maxSize = NSSize(width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        preview.isVerticallyResizable = true
        preview.autoresizingMask = [.width]
        preview.textContainer?.widthTracksTextView = true

        messageLabel.setAccessibilityIdentifier("pasteMarkdownMessage")
        pasteButton.target = self
        pasteButton.action = #selector(paste(_:))
        pasteButton.keyEquivalent = "\r"
        pasteButton.setAccessibilityIdentifier("pasteMarkdownPaste")
        cancelButton.target = self
        cancelButton.action = #selector(cancel(_:))
        cancelButton.keyEquivalent = "\u{1b}"
        let buttons = NSStackView(views: [NSView(), cancelButton, pasteButton])
        buttons.spacing = 8
        buttons.setHuggingPriority(.defaultLow, for: .horizontal)
        let stack = NSStackView(views: [scroll, messageLabel, buttons])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        stack.edgeInsets = NSEdgeInsets(top: 20, left: 20, bottom: 20, right: 20)
        stack.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -40),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 200),
            buttons.widthAnchor.constraint(equalTo: scroll.widthAnchor),
            messageLabel.widthAnchor.constraint(equalTo: scroll.widthAnchor),
        ])
        window.contentView = content
        show(.converting)
    }

    /// Attach to `parent`. `completion` runs once: the source to insert,
    /// or nil for Cancel or a dismissal.
    func begin(on parent: NSWindow, completion: @escaping @MainActor (String?) -> Void) {
        self.completion = completion
        parent.beginSheet(window)
    }

    func show(_ state: State) {
        self.state = state
        switch state {
        case .converting:
            preview.string = ""
            messageLabel.stringValue = "Converting…"
            messageLabel.isHidden = false
            pasteButton.isEnabled = false
        case .ready(let paste):
            preview.string = paste.markdown
            messageLabel.stringValue = Self.lossMessage(paste.losses) ?? ""
            messageLabel.isHidden = paste.losses.isEmpty
            pasteButton.isEnabled = true
        case .refused(let reason):
            preview.string = ""
            messageLabel.stringValue = reason
            messageLabel.isHidden = false
            pasteButton.isEnabled = false
            pasteButton.isHidden = true
        }
    }

    @objc func paste(_ sender: Any?) {
        guard case .ready(let paste) = state else { return }
        end(paste.markdown)
    }

    @objc func cancel(_ sender: Any?) { end(nil) }

    private func end(_ markdown: String?) {
        guard let completion else { return }
        self.completion = nil
        window.sheetParent?.endSheet(window)
        window.orderOut(nil)
        completion(markdown)
    }
}
