// The menu bar's one exception to AppKit's key routing (spec/08
// "Keyboard filing"): a matching item consumes its key even while it is
// disabled, so ⌘⌫ would never reach a text field once Move to Trash
// carries it. The bar lets it through while text is being edited.

import AppKit
import Carbon.HIToolbox

final class MainMenuBar: NSMenu {
    private static let shortcutModifiers: NSEvent.ModifierFlags = [.command, .option, .control, .shift]

    override func performKeyEquivalent(with event: NSEvent) -> Bool {
        if Self.isCommandDelete(event), Self.keyWindowEditsText() { return false }
        return super.performKeyEquivalent(with: event)
    }

    /// AppKit routes key equivalents on the main thread, but doesn't
    /// declare the override so.
    private static func keyWindowEditsText() -> Bool {
        MainActor.assumeIsolated { NSApplication.shared.keyWindow?.isEditingText == true }
    }

    private static func isCommandDelete(_ event: NSEvent) -> Bool {
        event.type == .keyDown && Int(event.keyCode) == kVK_Delete
            && event.modifierFlags.intersection(shortcutModifiers) == .command
    }
}

extension NSWindow {
    /// An editable text view, or a field's editor, has the keyboard.
    var isEditingText: Bool {
        (firstResponder as? NSText)?.isEditable == true
    }
}
