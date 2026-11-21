// Selected-file and inline-image publication for one compose session.
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
///
/// An inline image (spec/08 "Inline images") publishes the same way and
/// joins with a fresh Content-ID, but only while `admitsInlineImage`
/// says the body can take its reference; otherwise its row fails.
@MainActor
@Observable
public final class DraftAttachmentPublications {
    /// One file on its way into the draft.
    public struct Publication: Identifiable, Sendable {
        public let id: UUID
        public let filename: String
        /// Whether it joins as an inline image.
        public let isInline: Bool
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
    /// Whether a finished inline image may join now: the body is an
    /// editable Markdown source that can take its reference. Nil admits
    /// none.
    @ObservationIgnored public var admitsInlineImage: (@MainActor () -> Bool)?

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
        publish(
            filename: file.lastPathComponent, mediaType: Self.mediaType(of: file), inline: false
        ) { try await $0.publishDraftAttachment(file: file) }
    }

    /// Start publishing a PNG, JPEG or GIF file as an inline image; nil
    /// for another type or when this session takes no new work.
    @discardableResult
    public func insertImage(file: URL) -> UUID? {
        let mediaType = Self.mediaType(of: file)
        guard InlineImage.mediaTypes.contains(mediaType) else { return nil }
        return publish(filename: file.lastPathComponent, mediaType: mediaType, inline: true) {
            try await $0.publishDraftAttachment(file: file)
        }
    }

    /// Start publishing pasted image data as an inline image named
    /// "Image"; nil for another type or when this session takes no new
    /// work.
    @discardableResult
    public func insertImage(data: Data, mediaType: String) -> UUID? {
        guard InlineImage.mediaTypes.contains(mediaType),
            let ext = UTType(mimeType: mediaType)?.preferredFilenameExtension
        else { return nil }
        return publish(
            filename: "\(InlineImage.pastedName).\(ext)", mediaType: mediaType, inline: true
        ) { try await $0.publishDraftAttachment(bytes: data) }
    }

    private func publish(
        filename: String, mediaType: String, inline: Bool,
        _ command: @escaping @Sendable (any DraftStore) async throws -> DraftAttachmentObject
    ) -> UUID? {
        guard !isStopped, !model.isStopped, !model.isDiscarded, model.content != nil else {
            return nil
        }
        let id = UUID()
        publications.append(Publication(id: id, filename: filename, isInline: inline))
        let source = source
        tasks[id] = Task { [weak self] in
            let outcome: Result<DraftAttachmentObject, any Error>
            do {
                outcome = .success(try await command(source))
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
            let inline = publications[row].isInline
            if inline, admitsInlineImage?() != true {
                publications[row].failure = .unavailable(
                    reason: "The message body can't take an image now.")
                return
            }
            guard
                let index = model.appendAttachment(
                    .new(
                        object: object, filename: filename, mediaType: mediaType,
                        contentId: inline ? InlineImage.mintContentId() : nil))
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
