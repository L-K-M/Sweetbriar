// Dragging list rows to the sidebar (spec/08 "Keyboard filing"). The
// pasteboard only marks the drag as rows of a list: what is filed is
// the list's own capture, and only its window's sidebar takes it.

import AppKit
import SweetbriarBridge

enum MessageListDrag {
    static let pasteboardType = NSPasteboard.PasteboardType("ch.lkmc.Sweetbriar.message-row")

    static func item(occurrenceId: Int64) -> NSPasteboardItem {
        let item = NSPasteboardItem()
        item.setString(String(occurrenceId), forType: pasteboardType)
        return item
    }
}

/// Where the sidebar sends a drop of list rows.
@MainActor
protocol MessageDropTarget: AnyObject {
    func takesDrop(from source: Any?) -> Bool
    func drop(from source: Any?, onto mailbox: Mailbox) -> Bool
}
