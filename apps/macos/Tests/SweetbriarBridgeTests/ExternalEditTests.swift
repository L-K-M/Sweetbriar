// External edits' private files and watch (spec/08 "External editor",
// spec/10 "External editor files"), over the real file system with a
// fake editor that saves the way editors do.

import Foundation
import Testing

@testable import SweetbriarBridge

private func scratchRoot() -> URL {
    FileManager.default.temporaryDirectory
        .appendingPathComponent("sb-external-\(UUID().uuidString)", isDirectory: true)
        .appendingPathComponent(ExternalEditFiles.directoryName, isDirectory: true)
}

private func permissions(_ url: URL) throws -> Int {
    try #require(FileManager.default.attributesOfItem(atPath: url.path)[.posixPermissions] as? Int)
}

/// Saves the way most editors do: a new file renamed over the old one.
private func replacingSave(_ text: String, to file: URL) throws {
    try Data(text.utf8).write(to: file, options: .atomic)
}

@MainActor
private func watchEventually(_ predicate: @MainActor () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !predicate() {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(10))
    }
    return true
}

@Suite struct ExternalEditFileTests {
    /// A new `0700` directory named by a lowercase UUID, holding a `0600`
    /// file named for the draft's type, with the body as UTF-8.
    @Test func theBodyGoesToAPrivateFileNamedForTheFormat() throws {
        let root = scratchRoot()
        defer { ExternalEditFiles.purge(root.deletingLastPathComponent()) }
        let markdown = try ExternalEditFiles.create(body: "Grüße *du*", format: .markdown, in: root)
        let plain = try ExternalEditFiles.create(body: "plain", format: .plain, in: root)

        #expect(markdown.lastPathComponent == "draft.md" && plain.lastPathComponent == "draft.txt")
        let directory = markdown.deletingLastPathComponent()
        #expect(directory.deletingLastPathComponent().standardizedFileURL == root.standardizedFileURL)
        #expect(UUID(uuidString: directory.lastPathComponent) != nil)
        #expect(directory.lastPathComponent == directory.lastPathComponent.lowercased())
        #expect(directory != plain.deletingLastPathComponent())
        #expect(try permissions(root) == 0o700)
        #expect(try permissions(directory) == 0o700)
        #expect(try permissions(markdown) == 0o600)
        #expect(try Data(contentsOf: markdown) == Data("Grüße *du*".utf8))
    }

    @Test func aReadMustBeValidUTF8OfAtMost16MiB() throws {
        let root = scratchRoot()
        defer { ExternalEditFiles.purge(root.deletingLastPathComponent()) }
        let file = try ExternalEditFiles.create(body: "", format: .plain, in: root)
        #expect(ExternalEditFiles.read(file) == .success(""))
        try Data([0x48, 0xFF, 0xFE]).write(to: file)
        #expect(ExternalEditFiles.read(file) == .failure(.notUTF8))
        try Data(repeating: 0x61, count: ExternalEditFiles.maxBytes).write(to: file)
        #expect((try? ExternalEditFiles.read(file).get())?.utf8.count == ExternalEditFiles.maxBytes)
        try Data(repeating: 0x61, count: ExternalEditFiles.maxBytes + 1).write(to: file)
        #expect(ExternalEditFiles.read(file) == .failure(.tooLarge))
        ExternalEditFiles.remove(file)
        guard case .failure(.unreadable) = ExternalEditFiles.read(file) else {
            Issue.record("a missing file is unreadable")
            return
        }
    }

    /// Removing takes the edit's directory; the launch purge takes
    /// whatever is left, unread.
    @Test func removeAndPurgeDeleteTheEditsDirectories() throws {
        let root = scratchRoot()
        defer { ExternalEditFiles.purge(root.deletingLastPathComponent()) }
        let file = try ExternalEditFiles.create(body: "a", format: .plain, in: root)
        ExternalEditFiles.remove(file)
        #expect(!FileManager.default.fileExists(atPath: file.deletingLastPathComponent().path))
        #expect(FileManager.default.fileExists(atPath: root.path))

        let left = try ExternalEditFiles.create(body: "crash", format: .markdown, in: root)
        try Data([0xFF]).write(to: left)
        ExternalEditFiles.purge(root)
        #expect(!FileManager.default.fileExists(atPath: root.path))
    }
}

@MainActor @Suite(.serialized, .timeLimit(.minutes(1)))
struct ExternalEditWatchTests {
    private final class Received {
        var texts: [String] = []
    }

    private func watching(_ body: String) throws -> (ExternalEditWatch, Received, URL) {
        let root = scratchRoot()
        let file = try ExternalEditFiles.create(body: body, format: .markdown, in: root)
        let watch = ExternalEditWatch(file: file, original: body)
        let received = Received()
        watch.onChange = { received.texts.append($0) }
        watch.start()
        return (watch, received, root)
    }

    /// A replacing save, then an in-place one, each arrive once quiet.
    @Test func replacingAndInPlaceSavesArrive() async throws {
        let (watch, received, root) = try watching("Hello")
        defer { ExternalEditFiles.purge(root.deletingLastPathComponent()) }
        try replacingSave("Hello, replaced", to: watch.file)
        #expect(await watchEventually { received.texts == ["Hello, replaced"] })

        let handle = try FileHandle(forWritingTo: watch.file)
        try handle.truncate(atOffset: 0)
        try handle.write(contentsOf: Data("Hello, in place".utf8))
        try handle.close()
        #expect(await watchEventually { received.texts.last == "Hello, in place" })
        watch.discard()
    }

    /// An invalid save changes nothing and sets the failure until a
    /// later save fixes it; an unchanged one delivers nothing.
    @Test func anInvalidSaveChangesNothingUntilALaterSaveFixesIt() async throws {
        let (watch, received, root) = try watching("Hello")
        defer { ExternalEditFiles.purge(root.deletingLastPathComponent()) }
        try Data([0xFF, 0xFE]).write(to: watch.file, options: .atomic)
        #expect(await watchEventually { watch.failure == .notUTF8 })
        #expect(received.texts.isEmpty)

        try replacingSave("Hello", to: watch.file)
        #expect(await watchEventually { watch.failure == nil })
        #expect(received.texts.isEmpty, "the original text is no change")
        try replacingSave("Fixed", to: watch.file)
        #expect(await watchEventually { received.texts == ["Fixed"] })
        watch.discard()
    }

    /// Done reads a save made just before it, then deletes the file and
    /// its directory.
    @Test func finishReadsTheLastSaveAndDeletesTheFile() async throws {
        let (watch, received, root) = try watching("Hello")
        defer { ExternalEditFiles.purge(root.deletingLastPathComponent()) }
        try replacingSave("Last word", to: watch.file)
        #expect(await watch.finish() == nil)
        #expect(received.texts == ["Last word"])
        #expect(watch.isStopped)
        #expect(!FileManager.default.fileExists(atPath: watch.file.deletingLastPathComponent().path))
    }

    /// An invalid final read keeps the watch and the file.
    @Test func anInvalidFinalReadKeepsTheFile() async throws {
        let (watch, received, root) = try watching("Hello")
        defer { ExternalEditFiles.purge(root.deletingLastPathComponent()) }
        try Data([0xC3]).write(to: watch.file, options: .atomic)
        #expect(await watch.finish() == .notUTF8)
        #expect(!watch.isStopped && received.texts.isEmpty)
        #expect(FileManager.default.fileExists(atPath: watch.file.path))
        watch.discard()
        #expect(!FileManager.default.fileExists(atPath: watch.file.deletingLastPathComponent().path))
    }
}
