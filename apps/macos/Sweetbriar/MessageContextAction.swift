// One command in the list's context menu, acting on the rows the click
// targets (spec/08 "Selection").

import AppKit

struct MessageContextAction {
    let title: String
    let identifier: NSUserInterfaceItemIdentifier
    let enabled: Bool
    let run: @MainActor () -> Void
}

/// A menu item that carries its action.
@MainActor
final class MessageContextItem: NSMenuItem {
    private let run: @MainActor () -> Void

    init(_ action: MessageContextAction) {
        run = action.run
        super.init(title: action.title, action: #selector(choose(_:)), keyEquivalent: "")
        target = self
        identifier = action.identifier
        isEnabled = action.enabled
    }

    required init(coder: NSCoder) {
        fatalError("MessageContextItem is built in code")
    }

    @objc func choose(_ sender: Any?) {
        run()
    }
}
