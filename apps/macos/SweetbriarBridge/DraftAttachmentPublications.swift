// Selected-file attachment publication for one compose session.
import Foundation
import Observation
import UniformTypeIdentifiers

/// Publishes picked or dropped files for a `DraftModel` (spec/02, spec/07,
/// spec/08 "Attachments"): each object is durable before any revision
/// references it. A finished publication joins the content the editor holds
/// then, so edits made while the file streamed survive.
///
/// Cancelling an unbegun publication commits nothing. A begun one runs to
/// completion; a result nobody wants any more is dropped unreferenced.
/// Close and restore call `stopAndWait`: it fences new work and returns
/// only after every begun native command has finished.
@MainActor
@Observable
public final class DraftAttachmentPublications {
    /// One file on its way into the draft.
    public struct Publication: Identifiable, Sendable {
        public let id: UUID
        public let filename: String
        /// Why the publication failed; the row stays until dismissed.
        public fileprivate(set) var failure: CoreFailure?
    }

    /// Unfinished and failed publications, in the order they were chosen.
    public private(set) var publications: [Publication] = []
    public private(set) var isStopped = false
    /// True while a publication still has native work in flight.
    public var isPublishing: Bool { !tasks.isEmpty }

    /// Called with the attachment's index once it joined the content, so
    /// the window can register the add's undo.
    @ObservationIgnored public var onAttached: (@MainActor (Int) -> Void)?

    private let model: DraftModel
    private let source: any DraftStore
    @ObservationIgnored private var tasks: [UUID: Task<Void, Never>] = [:]

    public init(model: DraftModel, source: any DraftStore) {
        self.model = model
        self.source = source
    }

    /// Start publishing one file; nil when this session takes no new work.
    @discardableResult
    public func attach(file: URL) -> UUID? {
        guard !isStopped, !model.isStopped, !model.isDiscarded, model.content != nil else {
            return nil
        }
        let id = UUID()
        let filename = file.lastPathComponent
        let mediaType = Self.mediaType(of: file)
        publications.append(Publication(id: id, filename: filename))
        let source = source
        tasks[id] = Task { [weak self] in
            let outcome: Result<DraftAttachmentObject, any Error>
            do {
                outcome = .success(try await source.publishDraftAttachment(file: file))
            } catch {
                outcome = .failure(error)
            }
            self?.finish(id, outcome, filename: filename, mediaType: mediaType)
        }
        return id
    }

    /// Forget a publication: a failed row is cleared, an unbegun one is
    /// cancelled and a begun one's result will be dropped.
    public func dismiss(_ id: UUID) {
        publications.removeAll { $0.id == id }
        tasks[id]?.cancel()
    }

    /// Wait for every publication in flight to join the content or fail,
    /// without fencing: close waits here before its final save.
    public func settle() async {
        while let task = tasks.values.first { await task.value }
    }

    /// Fence: cancel unbegun publications and drop every later result.
    public func stop() {
        guard !isStopped else { return }
        isStopped = true
        publications.removeAll()
        for task in tasks.values { task.cancel() }
    }

    /// Fence, then await the begun native commands' actual outcome.
    public func stopAndWait() async {
        stop()
        await settle()
    }

    private func finish(
        _ id: UUID, _ outcome: Result<DraftAttachmentObject, any Error>,
        filename: String, mediaType: String
    ) {
        tasks[id] = nil
        guard !isStopped, let row = publications.firstIndex(where: { $0.id == id }) else { return }
        switch outcome {
        case .success(let object):
            guard
                let index = model.appendAttachment(
                    .new(object: object, filename: filename, mediaType: mediaType))
            else {
                publications[row].failure = .unavailable(reason: "The draft can no longer be edited.")
                return
            }
            publications.remove(at: row)
            onAttached?(index)
        case .failure(is CancellationError):
            publications.remove(at: row)
        case .failure(let error as CoreFailure):
            publications[row].failure = error
        case .failure(let error):
            publications[row].failure = .unavailable(reason: String(describing: error))
        }
    }

    /// The type a filename extension declares; unknown data otherwise.
    nonisolated static func mediaType(of file: URL) -> String {
        UTType(filenameExtension: file.pathExtension)?.preferredMIMEType
            ?? "application/octet-stream"
    }
}
