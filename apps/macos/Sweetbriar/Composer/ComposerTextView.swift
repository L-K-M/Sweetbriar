// The composer probe's editor (plan M0.11, spec/08 "Composer"): an
// `NSTextView` on TextKit 2 that live-styles Markdown source as
// presentation only — the display attributes ride on each paragraph's
// `NSTextParagraph`, never into the text storage — and whose
// formatting commands edit the source's `**`/`*` markers as one
// undoable edit. Nothing here touches `layoutManager`, which would
// drop the view into TextKit 1 compatibility mode for good.

import AppKit

/// The composer's editing mode (spec/08 "Composer"): plain text, or
/// Markdown with live styling. The probe window persists it under
/// one defaults key, defaulting to Markdown.
enum ComposerMode: String {
    case plain
    case markdown

    /// The defaults key the probe's mode persists under.
    private static let defaultsKey = "debug.composerProbe.mode"

    /// The stored mode — `.markdown` when unset or unknown.
    static var stored: ComposerMode {
        get {
            guard let raw = UserDefaults.standard.string(
                forKey: Self.defaultsKey
            ) else {
                return .markdown
            }
            return ComposerMode(rawValue: raw) ?? .markdown
        }
        set {
            UserDefaults.standard.set(
                newValue.rawValue,
                forKey: Self.defaultsKey
            )
        }
    }
}

/// The Markdown-aware text view. It builds its own TextKit 2 stack —
/// content storage, layout manager, container — so the content
/// storage's delegate can restyle each paragraph for display.
@MainActor
final class ComposerTextView: NSTextView {

    /// The TextKit 2 content storage backing the view — kept so the
    /// delegate can read the paragraph source and a mode switch can
    /// invalidate the elements it already answered.
    private let contentStorage = NSTextContentStorage()

    /// Plain or Markdown. Switching re-lays-out the document so the
    /// delegate runs again for every paragraph.
    var mode: ComposerMode = .markdown {
        didSet {
            guard mode != oldValue else { return }
            restyleDocument()
        }
    }

    /// Whether the view still runs TextKit 2 — the probe window's
    /// status line reads it, and the tests assert it stays true.
    /// `false` means a TextKit 1 API slipped in and forced
    /// compatibility mode.
    var isOnTextKit2: Bool {
        textLayoutManager != nil
    }

    init() {
        // The manual TextKit 2 stack (Apple's `init(frame:textContainer:)`
        // recipe): the content storage owns the text storage, the
        // layout manager lays out, the container bounds the text.
        // Handing the container to `init(frame:textContainer:)` puts
        // the text view on TextKit 2 (`textLayoutManager` non-nil).
        let layoutManager = NSTextLayoutManager()
        let container = NSTextContainer(
            size: NSSize(width: 0, height: CGFloat.greatestFiniteMagnitude)
        )
        layoutManager.textContainer = container
        contentStorage.addTextLayoutManager(layoutManager)
        super.init(frame: .zero, textContainer: container)

        contentStorage.delegate = self
        allowsUndo = true
        isRichText = false
        // Natural direction: right-to-left text is the composer's to
        // honor, never to force (spec/08).
        baseWritingDirection = .natural
        font = .systemFont(ofSize: NSFont.systemFontSize)
        setAccessibilityLabel("Message body")
    }

    required init?(coder: NSCoder) {
        fatalError("ComposerTextView is built in code")
    }

    // MARK: formatting commands

    /// Format ▸ Bold (⌘B), reached through the responder chain.
    @objc
    func toggleBoldMarkdown(_ sender: Any?) {
        applyMarkdownFormatting(.strong)
    }

    /// Format ▸ Italic (⌘I).
    @objc
    func toggleItalicMarkdown(_ sender: Any?) {
        applyMarkdownFormatting(.emphasis)
    }

    /// Compute the wrap-or-unwrap edit for each selected range and
    /// apply them last-to-first — earlier edits keep their
    /// coordinates — through `shouldChangeText`, the storage
    /// replacement and `didChangeText`, so the undo manager records
    /// one undoable edit for the whole command (spec/08).
    private func applyMarkdownFormatting(_ marker: MarkdownFormatting.Marker) {
        // No edits mid-composition: a storage change under marked
        // text would corrupt the input method's state.
        guard mode == .markdown, !hasMarkedText(), let textStorage else {
            return
        }

        var edits: [MarkdownFormatting.Edit] = []
        for value in selectedRanges {
            edits.append(MarkdownFormatting.edit(
                for: marker, in: string, selection: value.rangeValue
            ))
        }
        edits.sort { $0.range.location > $1.range.location }
        guard !edits.isEmpty else { return }

        if edits.count > 1 {
            undoManager?.beginUndoGrouping()
        }
        var newSelections: [NSValue] = []
        for edit in edits {
            guard shouldChangeText(
                in: edit.range, replacementString: edit.replacement
            ) else {
                continue
            }
            textStorage.replaceCharacters(in: edit.range, with: edit.replacement)
            didChangeText()
            newSelections.append(NSValue(range: edit.selection))
        }
        if edits.count > 1 {
            undoManager?.endUndoGrouping()
        }

        newSelections.sort { $0.rangeValue.location < $1.rangeValue.location }
        if !newSelections.isEmpty {
            selectedRanges = newSelections
        }
    }

    /// Bold and Italic are Markdown-only — off in plain mode.
    /// Anything else NSTextView validates itself.
    override func validateUserInterfaceItem(
        _ item: any NSValidatedUserInterfaceItem
    ) -> Bool {
        guard let action = item.action,
              action == #selector(toggleBoldMarkdown(_:))
                  || action == #selector(toggleItalicMarkdown(_:)) else {
            return super.validateUserInterfaceItem(item)
        }
        return mode == .markdown
    }

    /// Menu-item validation reaches the view through the same rule —
    /// covered separately in case AppKit prefers this entry point.
    override func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        guard let action = menuItem.action,
              action == #selector(toggleBoldMarkdown(_:))
                  || action == #selector(toggleItalicMarkdown(_:)) else {
            return super.validateMenuItem(menuItem)
        }
        return mode == .markdown
    }

    // MARK: marked text

    /// Committing marked text re-lays-out the document: a commit that
    /// changed no characters would otherwise leave the paragraph's
    /// element cached at whatever the delegate answered while the
    /// text was marked (spec/08).
    override func unmarkText() {
        super.unmarkText()
        restyleDocument()
    }

    /// Invalidate every element and layout so the delegate answers
    /// again for every paragraph — through TextKit 2's managers only.
    private func restyleDocument() {
        guard let layoutManager = textLayoutManager else { return }
        contentStorage.invalidateTextElements(for: layoutManager.documentRange)
        layoutManager.invalidateLayout(for: layoutManager.documentRange)
    }

    // MARK: display attributes

    /// The display layer's constants — the heading scale, the dimmed
    /// markers' color, the list hang.
    private enum Display {
        /// A marker's dim: still readable, clearly syntax.
        static let markerColor = NSColor.tertiaryLabelColor

        /// How far wrapped list text hangs right of the marker.
        static let listHeadIndent: CGFloat = 24

        /// A heading's point size, scaled off the body size — h1
        /// doubles it, h6 keeps it.
        static func headingFontSize(for level: Int, base: CGFloat) -> CGFloat {
            /// The h1…h6 multipliers.
            let factors: [CGFloat] = [2.0, 1.6, 1.4, 1.2, 1.1, 1.0]
            return factors[max(1, min(level, factors.count)) - 1] * base
        }

        /// The hanging indent a `.listItem` paragraph carries:
        /// marker at the margin, wraps aligned.
        static var listParagraphStyle: NSParagraphStyle {
            let style = NSMutableParagraphStyle()
            style.firstLineHeadIndent = 0
            style.headIndent = listHeadIndent
            return style
        }
    }

    /// The runs' display attributes, applied to the paragraph's
    /// presentation copy — the storage is never touched (spec/08).
    private func apply(
        _ runs: [MarkdownStyler.Run],
        to paragraph: NSMutableAttributedString
    ) {
        let baseSize = font?.pointSize ?? NSFont.systemFontSize
        for run in runs {
            switch run.style {
            case .heading(let level):
                paragraph.addAttribute(
                    .font,
                    value: NSFont.systemFont(
                        ofSize: Display.headingFontSize(
                            for: level, base: baseSize
                        ),
                        weight: .bold
                    ),
                    range: run.range
                )
            case .strong:
                addTrait(.boldFontMask, to: paragraph, range: run.range)
            case .emphasis:
                addTrait(.italicFontMask, to: paragraph, range: run.range)
            case .listItem:
                paragraph.addAttribute(
                    .paragraphStyle,
                    value: Display.listParagraphStyle,
                    range: run.range
                )
            case .marker:
                paragraph.addAttribute(
                    .foregroundColor,
                    value: Display.markerColor,
                    range: run.range
                )
            }
        }
    }

    /// A font trait over `range`, converting the font the run sits on
    /// — a heading's, say — rather than replacing it.
    private func addTrait(
        _ trait: NSFontTraitMask,
        to paragraph: NSMutableAttributedString,
        range: NSRange
    ) {
        let current = paragraph.attribute(
            .font, at: range.location, effectiveRange: nil
        ) as? NSFont ?? font ?? .systemFont(ofSize: NSFont.systemFontSize)
        paragraph.addAttribute(
            .font,
            value: NSFontManager.shared.convert(current, toHaveTrait: trait),
            range: range
        )
    }
}

// The protocol is nonisolated in the SDK; TextKit asks for paragraphs
// on the main thread, where the view lays out, so the conformance is
// main-actor isolated and `@preconcurrency` asserts that at run time.
extension ComposerTextView: @preconcurrency NSTextContentStorageDelegate {
    /// TextKit 2 asks for each paragraph's presentation: in Markdown
    /// mode, an `NSTextParagraph` carrying the styler's display
    /// attributes; in plain mode — and for any paragraph an input
    /// method holds marked — nil, so TextKit renders the source
    /// untouched. Marked text is never restyled mid-composition.
    func textContentStorage(
        _ textContentStorage: NSTextContentStorage,
        textParagraphWith range: NSRange
    ) -> NSTextParagraph? {
        guard mode == .markdown,
              let textStorage = contentStorage.textStorage,
              range.location >= 0,
              range.upperBound <= textStorage.length else {
            return nil
        }
        let marked = markedRange()
        guard marked.length == 0
                || NSIntersectionRange(marked, range).length == 0 else {
            return nil
        }
        let paragraph = NSMutableAttributedString(
            attributedString: textStorage.attributedSubstring(from: range)
        )
        apply(
            MarkdownStyler.styles(forParagraph: paragraph.string),
            to: paragraph
        )
        return NSTextParagraph(attributedString: paragraph)
    }
}
