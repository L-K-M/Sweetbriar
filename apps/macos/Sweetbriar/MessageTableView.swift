// The message list's table: AppKit's own, except that an arrow pressed
// with nothing selected may continue from where a selection left
// (spec/08 "Keyboard filing": "Afterwards").

import AppKit
import Carbon.HIToolbox

final class MessageTableView: NSTableView {
    enum Step {
        case previous
        case next
    }

    /// Offered a plain Up or Down while nothing is selected; true when
    /// it was handled.
    var resumeSelection: (@MainActor (Step) -> Bool)?

    private static let commandModifiers: NSEvent.ModifierFlags = [.command, .option, .control, .shift]

    override func keyDown(with event: NSEvent) {
        if selectedRowIndexes.isEmpty, let step = Self.step(event), resumeSelection?(step) == true {
            return
        }
        super.keyDown(with: event)
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
