// The reader's plain-text view (plan M1.5l, spec/08 "Reader"): an
// `NSTextView` on TextKit 2 showing a message's literal plain body —
// selectable, never editable, no Markdown interpretation and no
// automatic link or data detection: with quotes styled by depth, and
// a source mode that shows the same text monospaced ("View source").
// Nothing touches `layoutManager`, which would drop the view into
// TextKit 1 compatibility mode for good.

import AppKit

/// The read-only text view behind the reader's plain-text rendition:
/// the body stored verbatim, attributes applied for display only.
@MainActor
final class PlainTextReaderView: NSTextView {

    /// How the body is presented (spec/08 "Reader").
    enum Mode {
        /// The body with quotes styled by depth: a cycling color,
        /// the italic body font and an indent growing with depth.
        case text
        /// "View source": the same literal text, monospaced, unstyled.
        case source
    }

    /// The TextKit 2 content storage backing the view.
    private let contentStorage = NSTextContentStorage()

    /// The presentation mode: `.text` by default. Switching restyles
    /// the stored text; the text itself never changes.
    var mode: Mode = .text {
        didSet {
            guard mode != oldValue else { return }
            applyStyling()
        }
    }

    /// The message's literal plain body, stored verbatim; setting it
    /// restyles for the current mode.
    var text: String {
        get { string }
        set {
            textStorage?.setAttributedString(NSAttributedString(string: newValue))
            applyStyling()
        }
    }

    /// Whether the view still runs TextKit 2: `false` means a
    /// TextKit 1 API slipped in and forced compatibility mode.
    var isOnTextKit2: Bool {
        textLayoutManager != nil
    }

    init() {
        // The manual TextKit 2 stack (Apple's
        // `init(frame:textContainer:)` recipe): the content storage
        // owns the text storage, the layout manager lays out, the
        // container bounds the text. Handing the container to
        // `init(frame:textContainer:)` puts the view on TextKit 2.
        let layoutManager = NSTextLayoutManager()
        let container = NSTextContainer(
            size: NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude)
        )
        layoutManager.textContainer = container
        contentStorage.addTextLayoutManager(layoutManager)
        super.init(frame: .zero, textContainer: container)

        // Literal: nothing detected, nothing interpreted: it only
        // selects.
        isVerticallyResizable = true
        isHorizontallyResizable = false
        autoresizingMask = [.width]
        container.widthTracksTextView = true
        isEditable = false
        isSelectable = true
        isRichText = false
        // Edit → Find searches the shown body in the scroll view's find bar.
        usesFindBar = true
        isIncrementalSearchingEnabled = true
        isAutomaticLinkDetectionEnabled = false
        isAutomaticDataDetectionEnabled = false
        font = QuoteDepth.bodyFont
        setAccessibilityLabel("Message body")
    }

    required init?(coder: NSCoder) {
        fatalError("PlainTextReaderView is built in code")
    }

    // MARK: display styling

    /// The "View source" font.
    static let sourceFont = NSFont.monospacedSystemFont(
        ofSize: NSFont.systemFontSize, weight: .regular
    )

    /// The attributes every character carries in the current mode,
    /// before quote styling lands on top in `.text`.
    private var baseAttributes: [NSAttributedString.Key: Any] {
        [
            .font: mode == .text ? QuoteDepth.bodyFont : Self.sourceFont,
            .foregroundColor: NSColor.textColor,
        ]
    }

    /// Restyle the stored text for `mode`: attributes only; the
    /// string is never touched.
    private func applyStyling() {
        guard let textStorage else { return }
        let source = textStorage.string as NSString
        textStorage.beginEditing()
        if source.length > 0 {
            textStorage.setAttributes(
                baseAttributes,
                range: NSRange(location: 0, length: source.length)
            )
        }
        if mode == .text {
            styleQuotes(in: textStorage, source: source)
        }
        textStorage.endEditing()
    }

    /// Consume exactly one separator per line, including empty lines.
    /// CRLF is one separator; offsets stay in the original UTF-16 string.
    private func styleQuotes(in storage: NSTextStorage, source: NSString) {
        var location = 0
        for line in QuoteDepth.lines(source as String) {
            let lineRange = NSRange(
                location: location, length: (line.text as NSString).length
            )
            guard NSMaxRange(lineRange) <= source.length,
                  source.substring(with: lineRange) == line.text else {
                break
            }
            if line.depth > 0, lineRange.length > 0 {
                storage.addAttributes(
                    QuoteDepth.attributes(forDepth: line.depth),
                    range: source.paragraphRange(for: lineRange)
                )
            }
            location = NSMaxRange(lineRange)
            if location < source.length, source.character(at: location) == 0x0D {
                location += 1
                if location < source.length, source.character(at: location) == 0x0A {
                    location += 1
                }
            } else if location < source.length, source.character(at: location) == 0x0A {
                location += 1
            }
        }
    }

}
