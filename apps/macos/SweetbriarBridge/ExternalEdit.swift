// A compose body handed to an external editor (spec/08 "External
// editor", spec/10 "External editor files"): its private file, and the
// watch that reads the editor's saves back.
import Foundation

/// Where a compose source keeps external edits: `CoreBridge` answers
/// its cache root's `external-edit/`.
public protocol ExternalEditRootSource: Sendable {
    var externalEditRoot: URL { get }
}

extension CoreBridge: ExternalEditRootSource {
    public var externalEditRoot: URL {
        ExternalEditFiles.root(inCache: URL(fileURLWithPath: config.cacheRoot, isDirectory: true))
    }
}

/// The private files of external edits: one new `0700` directory per
/// edit under the cache root's `external-edit/`, named by a random
/// UUID, holding a `0600` `draft.md` or `draft.txt`. They are the only
/// draft content written outside the store.
public enum ExternalEditFiles {
    /// The directory under the cache root.
    public static let directoryName = "external-edit"
    /// A read larger than this changes nothing (spec/08).
    public static let maxBytes = 16 * 1024 * 1024

    /// Why a read of the edited file changes nothing.
    public enum ReadFailure: Error, Equatable, Sendable {
        case notUTF8
        case tooLarge
        case unreadable(String)

        public var message: String {
            switch self {
            case .notUTF8: "The edited file isn't valid UTF-8 text, so the message is unchanged."
            case .tooLarge: "The edited file is over 16 MB, so the message is unchanged."
            case .unreadable(let reason): "The edited file can't be read (\(reason)), so the message is unchanged."
            }
        }
    }

    /// `external-edit/` under `cacheRoot`.
    public static func root(inCache cacheRoot: URL) -> URL {
        cacheRoot.appendingPathComponent(directoryName, isDirectory: true)
    }

    /// Write `body` as UTF-8 to a new private file under `root`, named
    /// for `format`, never for the subject.
    public static func create(body: String, format: ComposeFormat, in root: URL) throws -> URL {
        let manager = FileManager.default
        let privateDirectory: [FileAttributeKey: Any] = [.posixPermissions: 0o700]
        try manager.createDirectory(at: root, withIntermediateDirectories: true, attributes: privateDirectory)
        let directory = root.appendingPathComponent(UUID().uuidString.lowercased(), isDirectory: true)
        try manager.createDirectory(at: directory, withIntermediateDirectories: false, attributes: privateDirectory)
        let file = directory.appendingPathComponent(format == .plain ? "draft.txt" : "draft.md")
        guard
            manager.createFile(
                atPath: file.path, contents: Data(body.utf8), attributes: [.posixPermissions: 0o600])
        else {
            try? manager.removeItem(at: directory)
            throw CoreFailure.unavailable(reason: "The body couldn't be written for the external editor.")
        }
        return file
    }

    /// The file's text: valid UTF-8 of at most `maxBytes`.
    public static func read(_ file: URL) -> Result<String, ReadFailure> {
        let data: Data
        do {
            let handle = try FileHandle(forReadingFrom: file)
            defer { try? handle.close() }
            data = try handle.read(upToCount: maxBytes + 1) ?? Data()
        } catch {
            return .failure(.unreadable(error.localizedDescription))
        }
        guard data.count <= maxBytes else { return .failure(.tooLarge) }
        guard let text = String(data: data, encoding: .utf8) else { return .failure(.notUTF8) }
        return .success(text)
    }

    /// Delete the edit's file and its directory.
    public static func remove(_ file: URL) {
        try? FileManager.default.removeItem(at: file.deletingLastPathComponent())
    }

    /// Delete whatever earlier runs left under `root`, unread: at launch,
    /// no edit is out yet.
    public static func purge(_ root: URL) {
        try? FileManager.default.removeItem(at: root)
    }
}

/// Watches one external edit (spec/08 "External editor"). Editors save
/// by replacing the file, so the watch follows the directory, and the
/// file itself for in-place saves; it reads once changes have been quiet
/// for 250 ms. Each valid read that differs from the last delivered text
/// reaches `onChange`; an invalid one sets `failure` until a later read
/// succeeds.
@MainActor
public final class ExternalEditWatch {
    public static let quiet: Duration = .milliseconds(250)
    /// How long Done, close or quit waits for quiet before its read.
    public static let finishLimit: Duration = .seconds(2)

    public let file: URL
    /// A changed valid read.
    public var onChange: (@MainActor (String) -> Void)?
    /// `failure` changed.
    public var onFailureChange: (@MainActor () -> Void)?
    /// The last read's failure; nil after a valid one.
    public private(set) var failure: ExternalEditFiles.ReadFailure? {
        didSet { if failure != oldValue { onFailureChange?() } }
    }
    public private(set) var isStopped = false

    private var delivered: String
    private var lastEvent = ContinuousClock.now
    private var directorySource: (any DispatchSourceFileSystemObject)?
    private var fileSource: (any DispatchSourceFileSystemObject)?
    private var pendingRead: Task<Void, Never>?

    /// `original` is the body the file was written from: reading it back
    /// unchanged delivers nothing.
    public init(file: URL, original: String) {
        self.file = file
        delivered = original
    }

    public func start() {
        guard !isStopped, directorySource == nil else { return }
        directorySource = source(for: file.deletingLastPathComponent(), events: .write)
        watchFile()
    }

    /// Wait for quiet (at most `finishLimit`), read once more and deliver
    /// a change. A valid read stops the watch and deletes the file; an
    /// invalid one keeps both and answers its failure.
    public func finish() async -> ExternalEditFiles.ReadFailure? {
        let deadline = ContinuousClock.now + Self.finishLimit
        while !isStopped {
            let quietAt = lastEvent + Self.quiet
            let now = ContinuousClock.now
            guard now < quietAt, now < deadline else { break }
            try? await Task.sleep(until: min(quietAt, deadline), clock: .continuous)
        }
        guard !isStopped else { return nil }
        pendingRead?.cancel()
        pendingRead = nil
        readNow()
        if let failure { return failure }
        discard()
        return nil
    }

    /// Stop watching and delete the file and its directory, unread.
    public func discard() {
        stop()
        ExternalEditFiles.remove(file)
    }

    /// Stop watching; the file stays.
    public func stop() {
        guard !isStopped else { return }
        isStopped = true
        pendingRead?.cancel()
        pendingRead = nil
        directorySource?.cancel()
        directorySource = nil
        fileSource?.cancel()
        fileSource = nil
    }

    private func source(
        for url: URL, events: DispatchSource.FileSystemEvent
    ) -> (any DispatchSourceFileSystemObject)? {
        let descriptor = open(url.path, O_EVTONLY)
        guard descriptor >= 0 else { return nil }
        let source = DispatchSource.makeFileSystemObjectSource(
            fileDescriptor: descriptor, eventMask: events, queue: .main)
        source.setEventHandler { [weak self] in
            MainActor.assumeIsolated { self?.changed() }
        }
        source.setCancelHandler { close(descriptor) }
        source.resume()
        return source
    }

    /// A replacing save leaves a new file behind the same name.
    private func watchFile() {
        fileSource?.cancel()
        fileSource = source(for: file, events: [.write, .extend, .delete, .rename])
    }

    private func changed() {
        guard !isStopped else { return }
        lastEvent = .now
        watchFile()
        pendingRead?.cancel()
        pendingRead = Task { [weak self] in
            try? await Task.sleep(for: Self.quiet)
            guard !Task.isCancelled else { return }
            self?.readNow()
        }
    }

    private func readNow() {
        switch ExternalEditFiles.read(file) {
        case .success(let text):
            failure = nil
            guard !text.utf8.elementsEqual(delivered.utf8) else { return }
            delivered = text
            onChange?(text)
        case .failure(let reason):
            failure = reason
        }
    }
}
