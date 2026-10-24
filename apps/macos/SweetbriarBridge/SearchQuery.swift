import Foundation

public enum SearchClauseReference: Sendable, Equatable {
    case account(String), mailbox(Int64), tag(String)
}

/// A presentation projection of core-validated JSON, never a text parser.
struct SearchQuery {
    typealias Node = [String: Any]
    var nodes: [Node]

    init(_ json: String) throws {
        guard let envelope = try JSONSerialization.jsonObject(with: Data(json.utf8)) as? Node,
              envelope["v"] as? Int == 1, let root = envelope["q"] as? Node else {
            throw CoreFailure.internalError(detail: "The core returned an unsupported search query.")
        }
        if let children = root["and"] as? [Node] {
            nodes = children
        } else {
            nodes = [root]
        }
    }

    static func encode(_ nodes: [Node]) throws -> String {
        let root: Node = nodes.count == 1 ? nodes[0] : ["and": nodes]
        let data = try JSONSerialization.data(
            withJSONObject: ["v": 1, "q": root], options: [.sortedKeys, .withoutEscapingSlashes]
        )
        return String(decoding: data, as: UTF8.self)
    }

    static func isChip(_ node: Node) -> Bool {
        node["text"] == nil && node["and"] == nil && node["or"] == nil && node["not"] == nil
    }

    static func reference(_ node: Node) -> SearchClauseReference? {
        if let id = node["account"] as? String { return .account(id) }
        if let id = node["mailbox"] as? NSNumber { return .mailbox(id.int64Value) }
        if let id = node["tag"] as? String { return .tag(id) }
        return nil
    }
}

public enum SearchParseSpan {
    /// Convert scalar-boundary UTF-8 offsets, including inside a grapheme cluster.
    public static func utf16Range(start: UInt32, end: UInt32, in text: String) -> NSRange? {
        let bytes = text.utf8
        guard start <= end,
              let first = bytes.index(bytes.startIndex, offsetBy: Int(start), limitedBy: bytes.endIndex),
              let last = bytes.index(first, offsetBy: Int(end - start), limitedBy: bytes.endIndex),
              let lower = first.samePosition(in: text.utf16),
              let upper = last.samePosition(in: text.utf16) else { return nil }
        return NSRange(
            location: text.utf16.distance(from: text.utf16.startIndex, to: lower),
            length: text.utf16.distance(from: lower, to: upper)
        )
    }
}
