import Foundation

/// Presentation only: retain markers, spaces and all message text literally.
enum QuoteDepth {
    struct Line: Equatable, Sendable {
        let text: String
        let depth: Int
    }

    static func lines(_ text: String) -> [Line] {
        // Normalize only line separators in the analysis; rendering uses the
        // original string. A final separator leaves a final empty line.
        text.replacingOccurrences(of: "\r\n", with: "\n")
            .replacingOccurrences(of: "\r", with: "\n")
            .components(separatedBy: "\n").map { line in
                var depth = 0
                for character in line {
                    if character == ">" {
                        depth += 1
                    } else if character != " " {
                        break
                    }
                }
                return Line(text: line, depth: depth)
            }
    }
}
