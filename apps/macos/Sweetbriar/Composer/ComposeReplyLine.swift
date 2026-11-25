import AppKit
import SwiftUI
import SweetbriarBridge

/// The accounts a reply's original may have arrived in (spec/07
/// `accounts()`). `CoreBridge` adopts it; tests substitute a fake.
protocol ReplyAccountSource: AnyObject, Sendable {
    func accounts() async throws -> [Account]
}

extension CoreBridge: ReplyAccountSource {}

/// "Reply to a message received in" and the receiving account's chip,
/// under a reply's header (spec/08 "Reply line"). It is one
/// accessibility element naming the account.
@MainActor
final class ComposeReplyLine: NSStackView {
    static let prefix = "Reply to a message received in"

    let label = NSTextField(labelWithString: ComposeReplyLine.prefix)
    let chip = NSHostingView(rootView: AccountChip(label: "", accountId: "", appearance: nil))

    override init(frame frameRect: NSRect) {
        super.init(frame: frameRect)
        orientation = .horizontal
        alignment = .centerY
        spacing = 6
        addArrangedSubview(label)
        addArrangedSubview(chip)
        setAccessibilityElement(true)
        setAccessibilityRole(.staticText)
        setAccessibilityIdentifier("composeReplyLine")
        isHidden = true
    }

    required init?(coder: NSCoder) { fatalError("ComposeReplyLine is built in code") }

    /// The chip's label and colors come from the account; the line's
    /// accessibility label names it in words.
    func show(accountId: String, accountName: String, appearance: Appearance?) {
        let chipView = AccountChip(label: accountName, accountId: accountId, appearance: appearance)
        if chip.rootView.label != chipView.label || chip.rootView.owner != chipView.owner
            || chip.rootView.appearance != chipView.appearance
        {
            chip.rootView = chipView
        }
        let spoken = "\(Self.prefix) \(accountName)"
        if accessibilityLabel() != spoken { setAccessibilityLabel(spoken) }
    }

    /// One element: the label and chip aren't read separately.
    override func accessibilityChildren() -> [Any]? { [] }
}
