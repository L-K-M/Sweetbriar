import Foundation

/// The reader's untrusted version-1 lease manifest (spec/07).
public struct RenditionManifest: Decodable, Sendable {
    public struct Resource: Decodable, Sendable {
        public let id: String
        public let file: String
        public let mediaType: String
        public let bytes: UInt64
        enum CodingKeys: String, CodingKey { case id, file, mediaType = "media_type", bytes }
    }

    public struct Link: Decodable, Sendable {
        public let href: String
        public let warnings: [String]
    }

    public struct Attachment: Decodable, Sendable {
        public let partID: String
        public let displayName: String
        public let mediaType: String
        public let bytes: UInt64?
        enum CodingKeys: String, CodingKey {
            case partID = "part_id", displayName = "display_name", mediaType = "media_type", bytes
        }
    }

    public static let maximumBytes = 4_000_000
    public let resources: [Resource]
    public let links: [Link]
    public let attachments: [Attachment]

    public enum Invalid: Error { case tooLarge, unsupportedVersion, unsafeResource }
    private enum CodingKeys: String, CodingKey { case v, resources, links, attachments }

    /// Use this entry point at byte boundaries; a Decoder cannot observe input size.
    public static func parse(_ data: Data) throws -> Self {
        guard data.count <= maximumBytes else { throw Invalid.tooLarge }
        return try JSONDecoder().decode(Self.self, from: data)
    }

    /// Bound the read itself, rather than allocating an oversized manifest first.
    public static func read(from url: URL) throws -> Self {
        let handle = try FileHandle(forReadingFrom: url)
        defer { try? handle.close() }
        return try parse(handle.read(upToCount: maximumBytes + 1) ?? Data())
    }

    public init(from decoder: any Decoder) throws {
        let values = try decoder.container(keyedBy: CodingKeys.self)
        guard try values.decode(Int.self, forKey: .v) == 1 else { throw Invalid.unsupportedVersion }
        resources = try values.decode([Resource].self, forKey: .resources)
        links = try values.decode([Link].self, forKey: .links)
        attachments = try values.decode([Attachment].self, forKey: .attachments)
        var files = Set<String>()
        var ids = Set<String>()
        for resource in resources {
            guard Self.isLeaf(resource.file), Self.isLeaf(resource.id),
                  files.insert(resource.file).inserted, ids.insert(resource.id).inserted,
                  ["image/png", "image/jpeg", "image/gif", "image/webp"].contains(resource.mediaType) else {
                throw Invalid.unsafeResource
            }
        }
    }

    private static func isLeaf(_ value: String) -> Bool {
        !value.isEmpty && value != "." && !value.contains("..") &&
            !value.contains("/") && !value.contains("\\") && !value.contains("\0")
    }
}

/// Immutable input for a pane. The model retains the corresponding body lease.
public struct HTMLRendition: Sendable {
    public let token: String
    public let directory: URL
    public let htmlFile: URL
    public let manifest: RenditionManifest

    public init(token: String, htmlFile: URL, manifest: RenditionManifest) {
        self.token = token
        self.htmlFile = htmlFile
        self.directory = htmlFile.deletingLastPathComponent()
        self.manifest = manifest
    }
}

/// Pure authorization: callers must recheck the ticket after asynchronous I/O.
public struct CIDResourceResolver: Sendable {
    public struct Resource: Sendable {
        public let fileURL: URL
        public let mediaType: String
    }

    private let rendition: HTMLRendition
    private let ticket: UInt64

    public init(rendition: HTMLRendition, ticket: UInt64) {
        self.rendition = rendition
        self.ticket = ticket
    }

    public func resolve(_ url: URL, currentTicket: UInt64) -> Resource? {
        guard ticket == currentTicket,
              let parts = URLComponents(url: url, resolvingAgainstBaseURL: false),
              parts.scheme == "sweetbriar-cid", parts.host == rendition.token,
              parts.user == nil, parts.password == nil, parts.port == nil,
              parts.query == nil, parts.fragment == nil,
              let resource = rendition.manifest.resources.first(where: { "/" + $0.id == parts.percentEncodedPath }) else {
            return nil
        }
        return Resource(
            fileURL: rendition.directory.appendingPathComponent(resource.file),
            mediaType: resource.mediaType
        )
    }
}

/// Authorization is exact to the current rendition's recorded destinations.
public enum ReaderLinkDecision: Sendable, Equatable {
    case open(URL)
    case confirm(URL, reasons: [String])
    case refuse

    public static func decide(_ url: URL, manifest: RenditionManifest) -> Self {
        guard ["http", "https", "mailto"].contains(url.scheme?.lowercased() ?? ""),
              let clicked = Destination(url.absoluteString) else { return .refuse }
        // One destination can be spelled several ways; its warnings are
        // the union of every listed spelling's.
        let listed = manifest.links.filter { Destination($0.href) == clicked }
        guard !listed.isEmpty else { return .refuse }
        var warnings: [String] = []
        for warning in listed.flatMap(\.warnings) where !warnings.contains(warning) {
            warnings.append(warning)
        }
        let reasons = warnings.map { warning in
            switch warning {
            case "text_domain": "The link text names a different domain."
            case "raw_ip": "The destination uses an IP address instead of a domain name."
            case "user_info": "The destination contains user information before the host name."
            default: "The destination has an unrecognized safety warning (\(warning))."
            }
        }
        return reasons.isEmpty ? .open(url) : .confirm(url, reasons: reasons)
    }
}

/// A link destination compared component by component. WebKit hands
/// clicks over as Foundation URLs, which percent-encode characters the
/// sanitizer's serializer keeps (`filter[status]` arrives as
/// `filter%5Bstatus%5D`), so escapes are undone, except a component's own
/// separators: `a%2Fb` and `a/b` are different destinations, and so are
/// `x%26y` and `x&y` in a query.
private struct Destination: Equatable {
    let scheme: String
    let user: String?
    let password: String?
    let host: String?
    let port: Int?
    let path: String
    let query: String?
    let fragment: String?

    init?(_ string: String) {
        guard let parts = URLComponents(string: string, encodingInvalidCharacters: true),
              let scheme = parts.scheme else { return nil }
        self.scheme = scheme.lowercased()
        user = parts.percentEncodedUser.map { Self.unescaped($0, keeping: ":@") }
        password = parts.percentEncodedPassword.map { Self.unescaped($0, keeping: ":@") }
        host = parts.percentEncodedHost.map { Self.unescaped($0, keeping: "").lowercased() }
        port = parts.port
        path = Self.unescaped(parts.percentEncodedPath, keeping: "/?#;")
        query = parts.percentEncodedQuery.map { Self.unescaped($0, keeping: "&=+;#") }
        fragment = parts.percentEncodedFragment.map { Self.unescaped($0, keeping: "") }
    }

    /// Undo percent-escapes byte by byte, except for `%` itself and the
    /// separators in `kept`, whose escapes stay (in upper-case hex).
    private static func unescaped(_ text: String, keeping kept: String) -> String {
        let kept = Set(kept.utf8).union([UInt8(ascii: "%")])
        var bytes: [UInt8] = []
        var input = Array(text.utf8)[...]
        while let byte = input.popFirst() {
            guard byte == UInt8(ascii: "%"), input.count >= 2,
                  let value = UInt8(String(decoding: input.prefix(2), as: UTF8.self), radix: 16)
            else {
                bytes.append(byte)
                continue
            }
            input = input.dropFirst(2)
            if kept.contains(value) {
                bytes.append(contentsOf: Array(String(format: "%%%02X", value).utf8))
            } else {
                bytes.append(value)
            }
        }
        return String(decoding: bytes, as: UTF8.self)
    }
}
