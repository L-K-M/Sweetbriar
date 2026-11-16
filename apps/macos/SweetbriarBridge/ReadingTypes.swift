// The reading document's bridge types (spec/08 Reader: Reading view):
// the app sees these, never the generated DTOs (spec/08 import
// boundary). CoreBridge maps the FFI answer field for field.

import Foundation

/// The semantic reading document, mirrored from the service DTO.
public struct ReadingDocument: Sendable, Equatable {
    public let sourcePartID: String
    public let version: UInt32
    public let blocks: [ReadingBlock]

    public init(sourcePartID: String, version: UInt32, blocks: [ReadingBlock]) {
        self.sourcePartID = sourcePartID
        self.version = version
        self.blocks = blocks
    }
}

public indirect enum ReadingBlock: Sendable, Equatable {
    case paragraph(partID: String, inlines: [ReadingInline])
    case heading(partID: String, level: ReadingHeadingLevel, inlines: [ReadingInline])
    case quote(partID: String, blocks: [ReadingBlock])
    case list(partID: String, ordered: Bool, start: Int64, reversed: Bool, items: [ReadingListItem])
    case preformatted(partID: String, text: String)
}

public enum ReadingHeadingLevel: Sendable, Equatable {
    case h1, h2, h3, h4, h5, h6
}

public struct ReadingListItem: Sendable, Equatable {
    public let partID: String
    /// The HTML `value` override, or the explicit plain-text number.
    public let number: Int64?
    public let blocks: [ReadingBlock]

    public init(partID: String, number: Int64?, blocks: [ReadingBlock]) {
        self.partID = partID
        self.number = number
        self.blocks = blocks
    }
}

public indirect enum ReadingInline: Sendable, Equatable {
    case text(partID: String, text: String)
    case emphasis(partID: String, strong: Bool, inlines: [ReadingInline])
    case link(partID: String, destination: String, inlines: [ReadingInline])
    case image(partID: String, alt: String, reference: ReadingImageReference)
    case lineBreak(partID: String)
}

public enum ReadingImageReference: Sendable, Equatable {
    case remote(url: String)
    case attachment(contentId: String, partId: String)
}
