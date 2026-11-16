import AppKit
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

    /// The body font every quote style italicizes. `NSFont` is immutable
    /// in practice; the quote styling only reads it.
    nonisolated(unsafe) static let bodyFont = NSFont.systemFont(ofSize: NSFont.systemFontSize)

    /// Points of indentation per quote depth.
    static let indent: CGFloat = 16

    /// The colors quotes cycle through by depth: depth 1 takes the first,
    /// deeper quotes wrap around.
    static let colors: [NSColor] = [
        .systemBlue, .systemGreen, .systemPurple, .systemOrange, .systemBrown,
    ]

    /// The attributes a quote at `depth` carries (spec/08): the depth's
    /// color, the italic body font and an indent growing with depth.
    /// Shared by the plain-text view and the Reading view's renderer.
    static func attributes(forDepth depth: Int) -> [NSAttributedString.Key: Any] {
        let indent = Self.indent * CGFloat(max(0, depth))
        let paragraph = NSMutableParagraphStyle()
        paragraph.firstLineHeadIndent = indent
        paragraph.headIndent = indent
        return [
            .font: NSFontManager.shared.convert(bodyFont, toHaveTrait: .italicFontMask),
            .foregroundColor: colors[max(0, depth - 1) % colors.count],
            .paragraphStyle: paragraph,
        ]
    }
}
