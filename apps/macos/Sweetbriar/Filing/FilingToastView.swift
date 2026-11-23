// The filing toast (spec/08 "Keyboard filing"): what was done, and
// Undo while the model offers it.

import AppKit
import SweetbriarBridge

@MainActor
final class FilingToastView: NSVisualEffectView {
    private static let cornerRadius: CGFloat = 8
    private static let inset: CGFloat = 10

    private let message = NSTextField(labelWithString: "")
    private let undoButton = NSButton(title: "Undo", target: nil, action: nil)
    private var shown: FilingToast.ID?

    /// Undo was pressed on the toast with this id.
    var onUndo: (@MainActor (FilingToast.ID) -> Void)?

    init() {
        super.init(frame: .zero)
        material = .popover
        blendingMode = .withinWindow
        state = .active
        wantsLayer = true
        layer?.cornerRadius = Self.cornerRadius
        layer?.masksToBounds = true
        translatesAutoresizingMaskIntoConstraints = false

        message.lineBreakMode = .byTruncatingMiddle
        message.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        message.setAccessibilityIdentifier("filingToastMessage")
        undoButton.target = self
        undoButton.action = #selector(undoPressed(_:))
        undoButton.setAccessibilityIdentifier("filingUndo")

        let row = NSStackView(views: [message, undoButton])
        row.spacing = Self.inset
        row.translatesAutoresizingMaskIntoConstraints = false
        addSubview(row)
        NSLayoutConstraint.activate([
            row.leadingAnchor.constraint(equalTo: leadingAnchor, constant: Self.inset),
            row.trailingAnchor.constraint(equalTo: trailingAnchor, constant: -Self.inset),
            row.topAnchor.constraint(equalTo: topAnchor, constant: Self.inset),
            row.bottomAnchor.constraint(equalTo: bottomAnchor, constant: -Self.inset),
        ])
        isHidden = true
    }

    required init?(coder: NSCoder) {
        fatalError("FilingToastView is built in code")
    }

    func show(_ toast: FilingToast?) {
        shown = toast?.id
        message.stringValue = toast?.message ?? ""
        message.toolTip = toast?.message
        undoButton.isHidden = toast?.canUndo != true
        isHidden = toast == nil
    }

    @objc private func undoPressed(_ sender: NSButton) {
        guard let shown else { return }
        onUndo?(shown)
    }
}
