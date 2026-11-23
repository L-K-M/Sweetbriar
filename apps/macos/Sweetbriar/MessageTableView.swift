// The message list's table: AppKit's own, except that an arrow pressed
// with nothing selected may continue from where a selection left
// (spec/08 "Keyboard filing": "Afterwards"), Return opens the selected
// message, and Space pages the reader (spec/08 "Message window").

import AppKit
import Carbon.HIToolbox

final class MessageTableView: NSTableView {
    enum Step {
        case previous
        case next
    }

    /// What Return and Space ask of the list's window.
    enum Command: Equatable {
        case open
        case page(ReaderPage)
    }

    /// Offered a plain Up or Down while nothing is selected; true when
    /// it was handled.
    var resumeSelection: (@MainActor (Step) -> Bool)?

    /// Offered Return, Enter, Space and ⇧Space; true when handled.
    var perform: (@MainActor (Command) -> Bool)?

    private static let commandModifiers: NSEvent.ModifierFlags = [.command, .option, .control, .shift]

    override func keyDown(with event: NSEvent) {
        if selectedRowIndexes.isEmpty, let step = Self.step(event), resumeSelection?(step) == true {
            return
        }
        if let command = Self.command(event), perform?(command) == true {
            return
        }
        super.keyDown(with: event)
    }

    /// Return or keypad Enter opens, Space pages down and ⇧Space up;
    /// with ⌘, ⌥ or ⌃ held they are left to the menus.
    static func command(_ event: NSEvent) -> Command? {
        let held = event.modifierFlags.intersection(commandModifiers)
        guard held.isDisjoint(with: [.command, .option, .control]) else { return nil }
        switch Int(event.keyCode) {
        case kVK_Return, kVK_ANSI_KeypadEnter:
            return held.isEmpty ? .open : nil
        case kVK_Space:
            return .page(held.contains(.shift) ? .up : .down)
        default:
            return nil
        }
    }

    private static func step(_ event: NSEvent) -> Step? {
        guard event.modifierFlags.isDisjoint(with: commandModifiers) else { return nil }
        switch Int(event.keyCode) {
        case kVK_UpArrow: return .previous
        case kVK_DownArrow: return .next
        default: return nil
        }
    }
}
