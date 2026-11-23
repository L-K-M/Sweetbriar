// The import sheet's inventory (spec/09 "In the app"): each source's
// mailbox name, message count and size, the first eight in inventory
// order, then how many more. Pure text, so the bridge's tests pin a
// locale.
//
//   Inbox: 1,204 messages, 3.2 MB
//   Archive 2019: 1 message, 2 KB
//   and 3 more sources

import Foundation

public enum ImportInventory {
    /// How many sources the sheet names before it counts the rest.
    public static let listed = 8

    /// One line per listed source, in inventory order, then a count of
    /// the sources left out; empty for no sources.
    public static func lines(_ sources: [ImportSource], locale: Locale = .current) -> [String] {
        let ordered = sources.sorted { $0.ordinal < $1.ordinal }
        var lines = ordered.prefix(listed).map { line($0, locale: locale) }
        let rest = ordered.count - listed
        if rest > 0 {
            lines.append("and \(rest) more \(rest == 1 ? "source" : "sources")")
        }
        return lines
    }

    private static func line(_ source: ImportSource, locale: Locale) -> String {
        let count = source.messages.formatted(.number.locale(locale))
        let size = Int64(clamping: source.bytes).formatted(.byteCount(style: .file).locale(locale))
        let noun = source.messages == 1 ? "message" : "messages"
        return "\(source.mailbox): \(count) \(noun), \(size)"
    }
}
