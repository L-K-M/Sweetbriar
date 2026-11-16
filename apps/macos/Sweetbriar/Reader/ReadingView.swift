// The reader's Reading view (plan M6.3b3, spec/08 "Reader"): the core's
// semantic document rendered natively by `ReadingRenderer` — read-only,
// selectable, links carrying their real destinations for AppKit's hover
// and pointing-hand affordances. A click is handed to the reader's shared
// link rule through `linkActivated` and handled here: returning true
// keeps AppKit from opening the URL itself. Nothing touches
// `layoutManager`, which would drop the view into TextKit 1 compatibility
// mode for good.

import AppKit
import SweetbriarBridge

/// The read-only text view behind the Reading view: the semantic document
/// as attributed text, never editable.
@MainActor
final class ReadingView: NSTextView {

    /// The TextKit 2 content storage backing the view.
    private let contentStorage = NSTextContentStorage()

    /// Called with a clicked link's destination; this view never opens it.
    var linkActivated: ((URL) -> Void)?

    init() {
        // Apple's manual TextKit 2 stack, as in PlainTextReaderView.
        let layoutManager = NSTextLayoutManager()
        let container = NSTextContainer(
            size: NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude)
        )
        layoutManager.textContainer = container
        contentStorage.addTextLayoutManager(layoutManager)
        super.init(frame: .zero, textContainer: container)

        isVerticallyResizable = true
        isHorizontallyResizable = false
        autoresizingMask = [.width]
        container.widthTracksTextView = true
        isEditable = false
        isSelectable = true
        isRichText = false
        isAutomaticLinkDetectionEnabled = false
        isAutomaticDataDetectionEnabled = false
        font = QuoteDepth.bodyFont
        delegate = self
        setAccessibilityLabel("Reading view")
    }

    required init?(coder: NSCoder) {
        fatalError("ReadingView is built in code")
    }

    /// Show the document's rendered text.
    func show(_ document: ReadingDocument) {
        textStorage?.setAttributedString(ReadingRenderer.attributedString(for: document))
    }

    /// Drop the shown document.
    func clear() {
        string = ""
    }
}

extension ReadingView: NSTextViewDelegate {
    /// Route every click through the reader's link rule and handle it here:
    /// returning true means AppKit never opens the URL itself.
    func textView(_ view: NSTextView, clickedOnLink link: Any, at index: Int) -> Bool {
        guard let url = link as? URL else { return true }
        linkActivated?(url)
        return true
    }
}
