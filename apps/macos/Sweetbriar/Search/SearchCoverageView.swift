// The strip under Search results (spec/08 "Search"): the coverage
// note's summary with a Details disclosure listing every count and
// the limitations. It shows the status of the rows on screen, as the
// list model publishes it; index controls attach here later.

import AppKit
import SweetbriarBridge

@MainActor
final class SearchCoverageView: NSView {
    /// The strip's padding on each side.
    private static let inset: CGFloat = 8

    private let summary = NSTextField(labelWithString: "")
    private let disclosure = NSButton()
    private let details = NSTextField(wrappingLabelWithString: "")

    override init(frame: NSRect) {
        super.init(frame: frame)

        for label in [summary, details] {
            label.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
            label.textColor = .secondaryLabelColor
            label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        }
        summary.lineBreakMode = .byTruncatingTail
        summary.setAccessibilityIdentifier("search.coverage.summary")
        details.setAccessibilityIdentifier("search.coverage.details")
        details.isHidden = true

        disclosure.setButtonType(.pushOnPushOff)
        disclosure.bezelStyle = .disclosure
        disclosure.title = ""
        disclosure.target = self
        disclosure.action = #selector(toggleDetails(_:))
        disclosure.setAccessibilityLabel("Search coverage details")
        disclosure.setAccessibilityIdentifier("search.coverage.disclosure")

        let header = NSStackView(views: [disclosure, summary])
        header.orientation = .horizontal
        header.alignment = .firstBaseline
        header.spacing = 4
        let stack = NSStackView(views: [header, details])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 4
        stack.edgeInsets = NSEdgeInsets(top: 6, left: Self.inset, bottom: 6, right: Self.inset)
        stack.translatesAutoresizingMaskIntoConstraints = false
        addSubview(stack)
        NSLayoutConstraint.activate([
            stack.topAnchor.constraint(equalTo: topAnchor),
            stack.leadingAnchor.constraint(equalTo: leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: trailingAnchor),
            stack.bottomAnchor.constraint(equalTo: bottomAnchor),
            // Long lines wrap or truncate inside the strip, never past it.
            header.widthAnchor.constraint(lessThanOrEqualTo: stack.widthAnchor, constant: -2 * Self.inset),
            details.widthAnchor.constraint(lessThanOrEqualTo: stack.widthAnchor, constant: -2 * Self.inset),
        ])
    }

    required init?(coder: NSCoder) {
        fatalError("SearchCoverageView is built in code")
    }

    /// Show `note`'s lines; the disclosure keeps its state.
    func show(_ note: SearchCoverageNote) {
        summary.stringValue = note.summary
        details.stringValue = note.details.joined(separator: "\n")
    }

    @objc
    private func toggleDetails(_ sender: NSButton) {
        details.isHidden = sender.state != .on
    }
}
