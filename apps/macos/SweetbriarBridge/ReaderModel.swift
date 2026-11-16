import Foundation
import Observation

/// Reader queries only; the app never opens the store or acquisition itself.
public protocol ReaderSource: Sendable {
    func messageForOccurrence(occurrenceId: Int64) async throws -> Int64
    func getMessage(messageId: Int64) async throws -> ReaderMessage
    func openBody(messageID: Int64, preferred: BodyKind) async throws -> BodyLease
    func openAttachment(messageID: Int64, partID: String) async throws -> AttachmentLease
    func releaseLease(_ leaseID: String)
    /// The local metadata commands (spec/07 Messages). A source that
    /// cannot perform them throws; `CoreBridge` implements them.
    func addTag(idempotencyKey: String, selection: MessageSelection, tag: String) async throws -> Receipt
    func removeTag(idempotencyKey: String, selection: MessageSelection, tag: String) async throws -> Receipt
    func setNote(idempotencyKey: String, messageId: Int64, text: String) async throws -> Receipt
}

extension ReaderSource {
    public func addTag(
        idempotencyKey: String, selection: MessageSelection, tag: String
    ) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "This source cannot apply tags.")
    }

    public func removeTag(
        idempotencyKey: String, selection: MessageSelection, tag: String
    ) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "This source cannot remove tags.")
    }

    public func setNote(
        idempotencyKey: String, messageId: Int64, text: String
    ) async throws -> Receipt {
        throw CoreFailure.unavailable(reason: "This source cannot save notes.")
    }
}

extension CoreBridge: ReaderSource {}

@MainActor
@Observable
public final class ReaderModel {
    public enum State {
        case empty
        case loading
        case plainText(message: ReaderMessage, text: String)
        case source(message: ReaderMessage, text: String)
        case html(message: ReaderMessage, rendition: HTMLRendition)
        case unavailable(CoreFailure)
        case failed(any Error)
    }

    public private(set) var state: State = .empty
    public private(set) var navigationTicket: UInt64 = 0
    public private(set) var occurrenceID: Int64?
    public private(set) var isShowingSource = false
    public var currentLease: BodyLease? { leaseOwner?.lease }
    public private(set) var manifest: RenditionManifest?
    private var leaseOwner: ReaderLeaseOwner?

    private let source: any ReaderSource
    private let preferred: BodyKind
    private var closed = false
    private var htmlAvailable = true

    public init(source: any ReaderSource, preferred: BodyKind = .plainText) {
        self.source = source
        self.preferred = preferred
    }

    /// Even selecting the same occurrence takes a new ticket. Callers may
    /// overlap these operations; cancellation is not the freshness check.
    public func show(occurrenceID: Int64) async {
        guard !closed else { return }
        manifest = nil
        self.occurrenceID = occurrenceID
        isShowingSource = false
        await load(occurrenceID: occurrenceID)
    }

    /// One action switches between the rendition and raw acquisition.
    public func showSource(_ enabled: Bool) async {
        guard !closed else { return }
        isShowingSource = enabled
        guard let occurrenceID else {
            invalidate()
            return
        }
        await load(occurrenceID: occurrenceID)
    }

    /// The pane calls this if the process-wide rule list cannot be compiled.
    /// Stale pane completions must never replace a newer selection.
    public func ruleListUnavailable(ticket: UInt64) async {
        guard !closed, ticket == navigationTicket else { return }
        htmlAvailable = false
        guard !isShowingSource, let occurrenceID else { return }
        await load(occurrenceID: occurrenceID)
    }

    public func clear() {
        invalidate()
        manifest = nil
        occurrenceID = nil
        isShowingSource = false
        state = .empty
    }

    /// Call when the pane closes, including while a request is outstanding.
    /// Deinitialization also releases a displayed lease.
    public func close() {
        closed = true
        clear()
    }

    /// The core keeps a handed-out, quarantined file until shutdown: an
    /// external app may still be opening it after this pane changes or closes.
    public func openAttachment(partID: String) async throws -> URL? {
        guard !closed, let message = currentMessage,
              manifest?.attachments.contains(where: { $0.partID == partID }) == true else { return nil }
        let ticket = navigationTicket
        let lease = try await source.openAttachment(messageID: message.messageId, partID: partID)
        guard !closed, ticket == navigationTicket else {
            source.releaseLease(lease.leaseId)
            return nil
        }
        return URL(fileURLWithPath: lease.filePath)
    }

    public var currentMessage: ReaderMessage? {
        switch state {
        case .plainText(let message, _), .source(let message, _), .html(let message, _): message
        default: nil
        }
    }

    /// Apply or remove `name` on the occurrence the user acted on
    /// (spec/07 Messages), then re-read its metadata if it is still
    /// shown. The caller names the target: another selection may be
    /// current by the time the write runs.
    public func setTag(
        _ name: String, applied: Bool, occurrenceID: Int64, messageID: Int64
    ) async throws {
        guard !closed else { return }
        let key = UUID().uuidString
        let selection = MessageSelection.occurrenceIds([occurrenceID])
        if applied {
            _ = try await source.addTag(idempotencyKey: key, selection: selection, tag: name)
        } else {
            _ = try await source.removeTag(idempotencyKey: key, selection: selection, tag: name)
        }
        try await refreshMessage(messageID)
    }

    /// Save the note of the message it was edited on; empty text
    /// removes it (spec/07).
    public func setNote(_ text: String, messageID: Int64) async throws {
        guard !closed else { return }
        _ = try await source.setNote(
            idempotencyKey: UUID().uuidString, messageId: messageID, text: text
        )
        try await refreshMessage(messageID)
    }

    /// Re-read the message's metadata after a write, if it is still the
    /// shown one.
    private func refreshMessage(_ messageID: Int64) async throws {
        guard currentMessage?.messageId == messageID else { return }
        let fresh = try await source.getMessage(messageId: messageID)
        guard currentMessage?.messageId == messageID else { return }
        switch state {
        case .plainText(_, let text):
            state = .plainText(message: fresh, text: text)
        case .source(_, let text):
            state = .source(message: fresh, text: text)
        case .html(_, let rendition):
            state = .html(message: fresh, rendition: rendition)
        default:
            break
        }
    }

    private func readManifest(_ lease: BodyLease) async throws -> RenditionManifest {
        try await Task.detached {
            guard let path = lease.manifestPath else { throw RenditionManifest.Invalid.unsafeResource }
            let htmlFile = URL(fileURLWithPath: lease.filePath)
            let manifestFile = URL(fileURLWithPath: path)
            guard manifestFile.deletingLastPathComponent() == htmlFile.deletingLastPathComponent() else {
                throw RenditionManifest.Invalid.unsafeResource
            }
            return try RenditionManifest.read(from: manifestFile)
        }.value
    }

    private func invalidate() {
        navigationTicket += 1
        leaseOwner = nil
    }

    private func load(occurrenceID: Int64) async {
        invalidate()
        let ticket = navigationTicket
        let kind: BodyKind = isShowingSource ? .source : (htmlAvailable ? preferred : .plainText)
        state = .loading
        do {
            let id = try await source.messageForOccurrence(occurrenceId: occurrenceID)
            guard ticket == navigationTicket else { return }
            let message = try await source.getMessage(messageId: id)
            guard ticket == navigationTicket else { return }
            let lease = try await source.openBody(messageID: id, preferred: kind)
            guard ticket == navigationTicket else {
                source.releaseLease(lease.leaseId)
                return
            }
            leaseOwner = ReaderLeaseOwner(lease: lease, source: source)
            if lease.kind == .sanitizedHtml {
                guard htmlAvailable, lease.manifestPath != nil else {
                    throw RenditionManifest.Invalid.unsafeResource
                }
                let parsed = try await readManifest(lease)
                guard ticket == navigationTicket else { return }
                manifest = parsed
                let rendition = HTMLRendition(
                    token: lease.leaseId, htmlFile: URL(fileURLWithPath: lease.filePath), manifest: parsed
                )
                state = .html(message: message, rendition: rendition)
                return
            }
            if lease.kind == .plainText, lease.manifestPath != nil {
                let parsed = try await readManifest(lease)
                guard ticket == navigationTicket else { return }
                manifest = parsed
            }
            // Source is arbitrary acquisition bytes, not necessarily UTF-8.
            // Replacement characters keep it inspectable without interpreting it.
            let text = try await Task.detached {
                let data = try Data(contentsOf: URL(fileURLWithPath: lease.filePath))
                return String(decoding: data, as: UTF8.self)
            }.value
            guard ticket == navigationTicket else { return }
            switch lease.kind {
            case .plainText: state = .plainText(message: message, text: text)
            case .source: state = .source(message: message, text: text)
            case .sanitizedHtml: preconditionFailure("HTML handled before text decoding")
            }
        } catch {
            guard ticket == navigationTicket else { return }
            leaseOwner = nil
            if let failure = error as? CoreFailure, case .unavailable = failure {
                state = .unavailable(failure)
            } else {
                state = .failed(error)
            }
        }
    }
}

/// Immutable ownership can be destroyed off the main actor under Swift 6.
/// Dropping the sole owner releases synchronously, including model teardown.
private final class ReaderLeaseOwner: Sendable {
    let lease: BodyLease
    private let source: any ReaderSource

    init(lease: BodyLease, source: any ReaderSource) {
        self.lease = lease
        self.source = source
    }

    deinit { source.releaseLease(lease.leaseId) }
}
