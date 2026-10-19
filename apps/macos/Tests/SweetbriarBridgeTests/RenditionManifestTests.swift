import Foundation
import Testing
@testable import SweetbriarBridge

private func manifestJSON(file: String = "r1.png", id: String = "r1", version: Int = 1, mediaType: String = "image/png") throws -> Data {
    try JSONSerialization.data(withJSONObject: [
        "v": version,
        "resources": [["id": id, "file": file, "media_type": mediaType, "bytes": 8]],
        "links": [["href": "https://example.test", "warnings": ["text_domain"]]],
        "attachments": [["part_id": "1.2", "display_name": "image", "media_type": "image/png", "bytes": NSNull()]],
    ])
}

@Test
func manifestDecodesCompleteVersionOne() throws {
    let manifest = try RenditionManifest.parse(manifestJSON())
    #expect(manifest.resources.first?.mediaType == "image/png")
    #expect(manifest.resources.first?.bytes == 8)
    #expect(manifest.links.first?.warnings == ["text_domain"])
    #expect(manifest.attachments.first?.partID == "1.2")
    #expect(manifest.attachments.first?.bytes == nil)
}

@Test(arguments: ["../r1.png", "x/y", "x\\y", "..", ".", "", "x..png", "x\0y"])
func manifestRejectsUnsafeFiles(file: String) throws {
    let data = try manifestJSON(file: file)
    #expect(throws: (any Error).self) { try RenditionManifest.parse(data) }
    // Decodable itself enforces semantic checks too.
    #expect(throws: (any Error).self) { try JSONDecoder().decode(RenditionManifest.self, from: data) }
}

@Test
func manifestRejectsVersionDuplicateFilesAndAmbiguousIDs() throws {
    #expect(throws: (any Error).self) { try RenditionManifest.parse(manifestJSON(version: 2)) }
    let resource = #"{"id":"r1","file":"r1.png","media_type":"image/png","bytes":8}"#
    for second in [
        resource.replacingOccurrences(of: #""id":"r1""#, with: #""id":"r2""#),
        resource.replacingOccurrences(of: "r1.png", with: "other.png"),
    ] {
        let data = Data("{\"v\":1,\"resources\":[\(resource),\(second)],\"links\":[],\"attachments\":[]}".utf8)
        #expect(throws: (any Error).self) { try RenditionManifest.parse(data) }
    }
}

@Test
func manifestEnforcesExactByteBoundaryAndBoundedFileRead() throws {
    var data = try manifestJSON()
    data.append(Data(repeating: 0x20, count: RenditionManifest.maximumBytes - data.count))
    _ = try RenditionManifest.parse(data)
    data.append(0x20)
    #expect(throws: RenditionManifest.Invalid.tooLarge) { try RenditionManifest.parse(data) }
    let file = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
    defer { try? FileManager.default.removeItem(at: file) }
    try data.write(to: file)
    #expect(throws: RenditionManifest.Invalid.tooLarge) { try RenditionManifest.read(from: file) }
}

@Test
func cidResolverAuthorizesOnlyCurrentLeaseManifestAndTicket() throws {
    let rendition = HTMLRendition(
        token: "token", htmlFile: URL(fileURLWithPath: "/cache/token/body.html"),
        manifest: try RenditionManifest.parse(manifestJSON())
    )
    let resolver = CIDResourceResolver(rendition: rendition, ticket: 7)
    let url = try #require(URL(string: "sweetbriar-cid://token/r1"))
    let resource = try #require(resolver.resolve(url, currentTicket: 7))
    #expect(resource.fileURL.path == "/cache/token/r1.png")
    #expect(resource.mediaType == "image/png")
    #expect(resolver.resolve(url, currentTicket: 8) == nil)
    for string in [
        "sweetbriar-cid://other/r1", "sweetbriar-cid://token/unknown", "sweetbriar-cid://token/r1.png",
        "sweetbriar-cid://token/../r1", "sweetbriar-cid://token/%2e%2e/r1", "sweetbriar-cid://token/%72%31",
        "sweetbriar-cid://token/r1/extra", "sweetbriar-cid://token/r1?x", "sweetbriar-cid://token/r1#x",
        "sweetbriar-cid://user@token/r1", "sweetbriar-cid://token:80/r1", "https://token/r1",
    ] {
        #expect(resolver.resolve(try #require(URL(string: string)), currentTicket: 7) == nil, "\(string)")
    }
}

@Test
func manifestRejectsUnsafeResourceIDAndActiveMedia() throws {
    #expect(throws: (any Error).self) { try RenditionManifest.parse(manifestJSON(id: "../r1")) }
    #expect(throws: (any Error).self) { try RenditionManifest.parse(manifestJSON(mediaType: "image/svg+xml")) }
}
