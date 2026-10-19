import Foundation
import Observation

/// Reader queries only; the app never opens the store or acquisition itself.
public protocol ReaderSource: Sendable {
    func messageForOccurrence(occurrenceId: Int64) async throws -> Int64
    func getMessage(messageId: Int64) async throws -> ReaderMessage
    func openBody(messageID: Int64, preferred: BodyKind) async throws -> BodyLease
    func releaseLease(_ leaseID: String)
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
        /// The actual kind is retained for M1.5m's HTML pane.
        case rendition(message: ReaderMessage, lease: BodyLease, text: String)
        case unavailable(CoreFailure)
        case failed(any Error)
    }

    public private(set) var state: State = .empty
    public private(set) var navigationTicket: UInt64 = 0
    public private(set) var occurrenceID: Int64?
    public private(set) var isShowingSource = false
    public var currentLease: BodyLease? { leaseOwner?.lease }
    private var leaseOwner: ReaderLeaseOwner?

    private let source: any ReaderSource
    private let preferred: BodyKind
    private var closed = false

    public init(source: any ReaderSource, preferred: BodyKind = .plainText) {
        self.source = source
        self.preferred = preferred
    }

    /// Even selecting the same occurrence takes a new ticket. Callers may
    /// overlap these operations; cancellation is not the freshness check.
    public func show(occurrenceID: Int64) async {
        guard !closed else { return }
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

    public func clear() {
        invalidate()
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

    private func invalidate() {
        navigationTicket += 1
        leaseOwner = nil
    }

    private func load(occurrenceID: Int64) async {
        invalidate()
        let ticket = navigationTicket
        let kind: BodyKind = isShowingSource ? .source : preferred
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
            case .sanitizedHtml: state = .rendition(message: message, lease: lease, text: text)
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
