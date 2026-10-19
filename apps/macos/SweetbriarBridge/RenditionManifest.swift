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
              let link = manifest.links.first(where: { $0.href == url.absoluteString }) else { return .refuse }
        let reasons = link.warnings.map { warning in
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
