import AppKit
import SwiftUI
import SweetbriarBridge

/// The recipient check's words and choices (spec/08 "Recipient check").
@MainActor
enum RecipientFencePresentation {
    static let notReady = "The recipient check starts once Sweetbriar has read your existing mail."

    /// "A", "A and B", "A, B and C".
    static func list(_ items: [String]) -> String {
        guard items.count > 1 else { return items.first ?? "" }
        return items.dropLast().joined(separator: ", ") + " and " + items[items.count - 1]
    }

    static func banner(address: String, accountNames: [String], sendingName: String) -> String {
        "\(address) has written to \(list(accountNames)) but never to \(sendingName). "
            + "Sweetbriar won't send to it from \(sendingName)."
    }

    static func allowConfirmation(address: String, sendingName: String) -> (message: String, information: String) {
        (
            "Allow sending to \(address) from \(sendingName)?",
            "Sweetbriar stops blocking this address for \(sendingName); Settings → Sending lists it."
        )
    }

    /// Send's tooltip and accessibility help while recipients are blocked.
    static func sendHelp(addresses: [String], sendingName: String) -> String {
        "Sweetbriar won't send to \(list(addresses)) from \(sendingName)."
    }

    /// What Use offers for a blocked address: for each account it wrote
    /// to that can send, that account's default sendable identity, as the
    /// cross-account banner chooses it.
    static func useIdentities(for recipient: BlockedRecipient, in identities: [ComposeIdentity]) -> [ComposeIdentity] {
        recipient.accountIds.compactMap { accountId in
            CrossAccountBannerModel.useIdentity(
                for: CrossAccountNotice(sourceAccountId: accountId, routeAccountId: "", ack: ""), in: identities)
        }
    }

    /// The canonical form the core matches by: the local part as
    /// written, the domain lowercased (spec/02).
    static func canonical(_ address: String) -> String {
        guard let at = address.lastIndex(of: "@") else { return address }
        return String(address[..<at]) + "@" + address[address.index(after: at)...].lowercased()
    }
}

/// One blocked recipient's banner, in the cross-account banner's warning
/// style: the sentence, a chip per account the address wrote to, Use and
/// Allow.
@MainActor
final class RecipientFenceBanner: NSStackView {
    struct Content: Equatable {
        struct Use: Equatable {
            let identityId: String
            let accountName: String
        }

        let address: String
        let text: String
        /// Each account the address wrote to: its id and name.
        let accounts: [(id: String, name: String)]
        let uses: [Use]
        let sendingName: String

        static func == (lhs: Content, rhs: Content) -> Bool {
            lhs.address == rhs.address && lhs.text == rhs.text && lhs.uses == rhs.uses
                && lhs.sendingName == rhs.sendingName
                && lhs.accounts.map(\.id) == rhs.accounts.map(\.id)
                && lhs.accounts.map(\.name) == rhs.accounts.map(\.name)
        }
    }

    let content: Content
    let label = NSTextField(wrappingLabelWithString: "")
    let symbol = NSImageView(
        image: NSImage(systemSymbolName: "exclamationmark.triangle.fill", accessibilityDescription: "Warning")
            ?? NSImage())
    let chips = NSStackView()
    /// Use, for one account that can send.
    let useButton = NSButton(title: "", target: nil, action: nil)
    /// Use, as a menu, for several.
    let useMenu = NSPopUpButton(frame: .zero, pullsDown: true)
    let allowButton = NSButton(title: "", target: nil, action: nil)
    private let onUse: @MainActor (String) -> Void
    private let onAllow: @MainActor (String) -> Void

    init(
        _ content: Content, appearance: Appearance?, onUse: @escaping @MainActor (String) -> Void,
        onAllow: @escaping @MainActor (String) -> Void
    ) {
        self.content = content
        self.onUse = onUse
        self.onAllow = onAllow
        super.init(frame: .zero)
        orientation = .horizontal
        alignment = .centerY
        spacing = 8
        edgeInsets = NSEdgeInsets(top: 6, left: 8, bottom: 6, right: 8)
        setAccessibilityIdentifier("recipientFenceBanner")
        symbol.contentTintColor = .labelColor
        symbol.setContentHuggingPriority(.required, for: .horizontal)
        label.stringValue = content.text
        label.textColor = .labelColor
        label.setContentHuggingPriority(.defaultLow, for: .horizontal)
        label.setContentCompressionResistancePriority(.defaultLow, for: .horizontal)
        label.setAccessibilityIdentifier("recipientFenceLabel")
        chips.orientation = .horizontal
        chips.spacing = 4
        for account in content.accounts {
            chips.addArrangedSubview(
                NSHostingView(rootView: AccountChip(label: account.name, accountId: account.id, appearance: appearance)))
        }
        let text = NSStackView(views: [label, chips])
        text.orientation = .vertical
        text.alignment = .leading
        text.spacing = 4
        addArrangedSubview(symbol)
        addArrangedSubview(text)
        if content.uses.count == 1, let use = content.uses.first {
            useButton.title = "Use \(use.accountName)"
            useButton.target = self
            useButton.action = #selector(useOne)
            useButton.setAccessibilityIdentifier("recipientFenceUse")
            addArrangedSubview(useButton)
        } else if content.uses.count > 1 {
            useMenu.addItem(withTitle: "Use")
            for use in content.uses {
                let item = NSMenuItem(title: "Use \(use.accountName)", action: #selector(useChosen(_:)), keyEquivalent: "")
                item.target = self
                item.representedObject = use.identityId
                useMenu.menu?.addItem(item)
            }
            useMenu.setAccessibilityIdentifier("recipientFenceUseMenu")
            addArrangedSubview(useMenu)
        }
        allowButton.title = "Allow for \(content.sendingName)…"
        allowButton.target = self
        allowButton.action = #selector(allow)
        allowButton.setAccessibilityIdentifier("recipientFenceAllow")
        addArrangedSubview(allowButton)
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    /// Use and Allow follow the window's busy state.
    func setControlsEnabled(_ enabled: Bool) {
        for control in [useButton, useMenu, allowButton] as [NSControl] where control.isEnabled != enabled {
            control.isEnabled = enabled
        }
    }

    @objc private func useOne() {
        guard let use = content.uses.first else { return }
        onUse(use.identityId)
    }

    @objc func useChosen(_ sender: NSMenuItem) {
        guard let identityId = sender.representedObject as? String else { return }
        onUse(identityId)
    }

    @objc private func allow() { onAllow(content.address) }
}
