// The strip under Search results (spec/08 "Search"): the coverage
// note's summary with a Details disclosure holding the index controls
// — Rebuild, Cancel, Resume, Retry Now — every count and the
// limitations. It shows the status of the rows on screen, or
// standalone coverage while Search can't open.

import AppKit
import SweetbriarBridge

@MainActor
final class SearchCoverageView: NSView {
    /// The strip's padding on each side.
    private static let inset: CGFloat = 8

    private let summary = NSTextField(labelWithString: "")
    private let disclosure = NSButton()
    private let details = NSTextField(wrappingLabelWithString: "")
    private let failure = NSTextField(wrappingLabelWithString: "")
    private let controls = NSStackView()
    private let detailStack = NSStackView()
    private var buttons: [(control: IndexControls, command: IndexControlModel.Command, button: NSButton)] = []
    /// What the last `show` offered, and whether a command was pending:
    /// a key equivalent can reach a hidden or disabled button.
    private var offered: IndexControls = []
    private var busy = false

    /// A control was clicked.
    var onCommand: ((IndexControlModel.Command) -> Void)?

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
        failure.font = .systemFont(ofSize: NSFont.smallSystemFontSize)
        failure.textColor = .systemRed
        failure.setAccessibilityIdentifier("search.coverage.error")
        failure.isHidden = true

        controls.orientation = .horizontal
        controls.spacing = 8
        for (control, command, title, id) in [
            (IndexControls.rebuild, IndexControlModel.Command.rebuild, "Rebuild Index", "rebuild"),
            (.cancel, .cancel, "Cancel", "cancel"),
            (.resume, .resume, "Resume", "resume"),
            (.retryNow, .retryNow, "Retry Now", "retry"),
        ] {
            let button = NSButton(title: title, target: self, action: #selector(commandClicked(_:)))
            button.controlSize = .small
            button.setAccessibilityIdentifier("search.coverage.\(id)")
            button.isHidden = true
            controls.addArrangedSubview(button)
            buttons.append((control, command, button))
        }
        // Cancel answers ⌘. like any Mac cancel.
        if let cancel = buttons.first(where: { $0.command == .cancel })?.button {
            cancel.keyEquivalent = "."
            cancel.keyEquivalentModifierMask = .command
        }

        detailStack.orientation = .vertical
        detailStack.alignment = .leading
        detailStack.spacing = 4
        for view in [controls, failure, details] {
            detailStack.addArrangedSubview(view)
        }
        detailStack.isHidden = true

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
        let stack = NSStackView(views: [header, detailStack])
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
            detailStack.widthAnchor.constraint(lessThanOrEqualTo: stack.widthAnchor, constant: -2 * Self.inset),
        ])
    }

    required init?(coder: NSCoder) {
        fatalError("SearchCoverageView is built in code")
    }

    /// Show `note`'s lines, the `offered` controls — all disabled
    /// while a command is `busy` — and the last control failure. The
    /// disclosure keeps its state.
    func show(_ note: SearchCoverageNote, offered: IndexControls, busy: Bool, failure message: String?) {
        self.offered = offered
        self.busy = busy
        summary.stringValue = note.summary
        details.stringValue = note.details.joined(separator: "\n")
        for entry in buttons {
            entry.button.isHidden = !offered.contains(entry.control)
            entry.button.isEnabled = !busy
        }
        failure.stringValue = message ?? ""
        failure.isHidden = message == nil
    }

    /// Open the details: while Search can't open, its controls are the
    /// only way on.
    func disclose() {
        disclosure.state = .on
        detailStack.isHidden = false
    }

    @objc
    private func toggleDetails(_ sender: NSButton) {
        detailStack.isHidden = sender.state != .on
    }

    @objc
    private func commandClicked(_ sender: NSButton) {
        guard let entry = buttons.first(where: { $0.button === sender }),
              offered.contains(entry.control), !busy, !detailStack.isHidden
        else { return }
        onCommand?(entry.command)
    }
}
