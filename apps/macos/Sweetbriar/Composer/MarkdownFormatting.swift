// The composer's ⌘B/⌘I transformation (plan M0.11, spec/08
// "Composer"): formatting commands edit the Markdown source itself —
// wrap the selection in `**`/`*` markers, or remove them again when
// the selection is already wrapped — as one undoable edit computed
// here. Pure and Foundation-only so it tests on Linux.

import Foundation

/// Wrap-or-unwrap edits for the composer's formatting commands.
enum MarkdownFormatting {
    /// Which marker the command toggles.
    enum Marker {
        /// `**…**` — ⌘B.
        case strong
        /// `*…*` — ⌘I.
        case emphasis

        /// The source characters inserted or removed.
        var text: String {
            switch self {
            case .strong:
                return "**"
            case .emphasis:
                return "*"
            }
        }
    }

    /// The one edit a formatting command applies through
    /// `shouldChangeText`/`didChangeText` so the undo manager records
    /// a single undoable change.
    struct Edit: Equatable {
        /// The text range the replacement covers, in the pre-edit
        /// string's UTF-16 units.
        let range: NSRange
        /// What replaces `range`.
        let replacement: String
        /// The selection after the edit, in the post-edit string's
        /// UTF-16 units.
        let selection: NSRange
    }

    /// The edit that toggles `marker` around `selection` in `text`:
    /// - markers inside the selection (`|**x**|`) unwrap;
    /// - markers just outside it (`**|x|**`) unwrap too;
    /// - an empty selection inserts a marker pair with the caret
    ///   between;
    /// - anything else wraps the selection, and the new selection
    ///   covers the wrapped span so ⌘B again unwraps.
    static func edit(
        for marker: Marker,
        in text: String,
        selection: NSRange
    ) -> Edit {
        let ns = text as NSString
        let selection = NSIntersectionRange(
            selection, NSRange(location: 0, length: ns.length)
        )
        let m = marker.text.utf16.count

        guard selection.length > 0 else {
            return Edit(
                range: selection,
                replacement: marker.text + marker.text,
                selection: NSRange(location: selection.location + m, length: 0)
            )
        }

        let inner = ns.substring(with: selection)

        // Markers inside the selection: `**x**` → `x`.
        if inner.hasPrefix(marker.text), inner.hasSuffix(marker.text),
           inner.utf16.count > 2 * m {
            return Edit(
                range: selection,
                replacement: (inner as NSString)
                    .substring(with: NSRange(
                        location: m, length: inner.utf16.count - 2 * m
                    )),
                selection: NSRange(
                    location: selection.location,
                    length: selection.length - 2 * m
                )
            )
        }

        // Markers just outside the selection: `**x**` → `x`, and the
        // selection follows the word left by the dropped markers.
        if selection.location >= m,
           selection.location + selection.length + m <= ns.length,
           ns.substring(with: NSRange(
               location: selection.location - m, length: m
           )) == marker.text,
           ns.substring(with: NSRange(
               location: selection.location + selection.length, length: m
           )) == marker.text {
            return Edit(
                range: NSRange(
                    location: selection.location - m,
                    length: selection.length + 2 * m
                ),
                replacement: inner,
                selection: NSRange(
                    location: selection.location - m,
                    length: selection.length
                )
            )
        }

        // A plain selection wraps; it grows to cover the wrapped span.
        return Edit(
            range: selection,
            replacement: marker.text + inner + marker.text,
            selection: NSRange(
                location: selection.location,
                length: selection.length + 2 * m
            )
        )
    }
}
