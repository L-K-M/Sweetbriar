// The reader header's metadata controls (spec/08 Reader): a tag picker
// and a note editor that saves on end-editing. The writes belong to
// `ReaderModel`.

import AppKit
import SweetbriarBridge

/// The message's tags as removable chips and a field adding one (spec/08).
@MainActor
final class TagPickerView: NSView {
    /// Add the name in the field.
    var onAdd: (@MainActor (String) -> Void)?
    /// Remove one of the message's tags.
    var onRemove: (@MainActor (ReaderTag) -> Void)?

    private let chips = NSStackView()
    private let field = NSTextField(string: "")
    private let addButton = NSButton(title: "Add", target: nil, action: nil)

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        chips.orientation = .horizontal
        chips.spacing = 4
        chips.alignment = .centerY
        chips.setAccessibilityIdentifier("readerTags")

        field.placeholderString = "Add tag"
        field.setAccessibilityIdentifier("readerTag.field")
        field.setAccessibilityLabel("Add tag")
        field.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        field.target = self
        field.action = #selector(addFromField(_:))

        addButton.bezelStyle = .rounded
        addButton.controlSize = .small
        addButton.target = self
        addButton.action = #selector(addFromField(_:))
        addButton.setAccessibilityIdentifier("readerTag.add")
        addButton.setAccessibilityLabel("Add tag")

        let row = NSStackView(views: [chips, field, addButton])
        row.orientation = .horizontal
        row.spacing = 6
        row.alignment = .centerY
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor),
            row.trailingAnchor.constraint(lessThanOrEqualTo: trailingAnchor),
            row.topAnchor.constraint(equalTo: topAnchor),
            row.bottomAnchor.constraint(equalTo: bottomAnchor),
            field.widthAnchor.constraint(greaterThanOrEqualToConstant: 80),
            chips.widthAnchor.constraint(lessThanOrEqualToConstant: 240),
        ])
        setAccessibilityIdentifier("readerTag.picker")
    }

    required init?(coder: NSCoder) {
        fatalError("TagPickerView is built in code")
    }

    /// Show `tags` as chips, in the message's order.
    func configure(tags: [ReaderTag]) {
        for chip in chips.arrangedSubviews { chip.removeFromSuperview() }
        for tag in tags {
            let chip = TagChipButton(title: tag.name, target: self, action: #selector(removeChip(_:)))
            chip.bezelStyle = .rounded
            chip.controlSize = .small
            chip.readerTag = tag
            chip.toolTip = "Remove tag \(tag.name)"
            chip.setAccessibilityIdentifier("readerTag.remove.\(tag.tagId)")
            chip.setAccessibilityLabel("Remove tag \(tag.name)")
            chips.addArrangedSubview(chip)
        }
    }

    /// The field's text, for tests and for the add action.
    var text: String {
        get { field.stringValue }
        set { field.stringValue = newValue }
    }

    /// The shown tag names, for tests.
    var tagNames: [String] {
        chips.arrangedSubviews.compactMap { ($0 as? TagChipButton)?.readerTag }.map(\.name)
    }

    @objc private func addFromField(_ sender: Any?) {
        let name = field.stringValue.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !name.isEmpty else { return }
        field.stringValue = ""
        onAdd?(name)
    }

    @objc private func removeChip(_ sender: TagChipButton) {
        guard let tag = sender.readerTag else { return }
        onRemove?(tag)
    }
}

/// A tag chip holds its tag; `NSButton` has no represented object.
@MainActor
private final class TagChipButton: NSButton {
    var readerTag: ReaderTag?
}

/// A multi-line note editor (spec/08): saved when editing ends; empty
/// text clears the note (spec/07).
@MainActor
final class NoteEditorView: NSScrollView {
    /// The note's Markdown source, saved when editing ends.
    var onEndEditing: (@MainActor (String) -> Void)?

    let textView = NoteTextView()
    /// Whether it is being edited: the reader leaves the text alone.
    var isEditing: Bool { textView.isEditingNote }

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        textView.isEditable = true
        textView.isSelectable = true
        textView.isRichText = false
        textView.allowsUndo = true
        textView.onResign = { [weak self] in
            guard let self else { return }
            self.onEndEditing?(self.textView.string)
        }
        textView.identifier = NSUserInterfaceItemIdentifier("readerNote")
        textView.setAccessibilityIdentifier("readerNote")
        textView.setAccessibilityLabel("Note")
        documentView = textView
        hasVerticalScroller = true
        autohidesScrollers = true
        borderType = .bezelBorder
        focusRingType = .none
        setAccessibilityIdentifier("readerNote")
    }

    required init?(coder: NSCoder) {
        fatalError("NoteEditorView is built in code")
    }

    var text: String {
        get { textView.string }
        set { textView.string = newValue }
    }

}

/// A text view that reports when it stops being the first responder.
@MainActor
final class NoteTextView: NSTextView {
    var onResign: (() -> Void)?
    private(set) var isEditingNote = false

    override func becomeFirstResponder() -> Bool {
        let accepted = super.becomeFirstResponder()
        if accepted { isEditingNote = true }
        return accepted
    }

    override func resignFirstResponder() -> Bool {
        let accepted = super.resignFirstResponder()
        if accepted, isEditingNote {
            isEditingNote = false
            onResign?()
        }
        return accepted
    }
}
