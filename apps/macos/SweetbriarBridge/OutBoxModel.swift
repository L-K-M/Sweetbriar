// `OutBoxModel`: the sidebar's Out Box (spec/08 "Out Box"), free of
// AppKit so the bridge's tests exercise it.
//
// It reads `out_box()`, one reload at a time; a reload asked for
// meanwhile runs once after it. A failed reload keeps the rows it
// showed. Each action runs alone under one fresh key and always
// reloads; an action is refused while archive writes are unavailable,
// like the composer's. `retire()` ends the list for an old archive: a
// result that arrives later is dropped and no row offers an action.

import Foundation
import Observation

@MainActor
@Observable
public final class OutBoxModel {
    /// One Out Box row as the list shows it (spec/08 "Out Box").
    public struct Row: Identifiable, Equatable, Sendable {
        /// The attempt's id.
        public var id: String
        /// The draft the attempt belongs to.
        public var draftId: String
        /// The subject, or "No Subject".
        public var subject: String
        /// To, then Cc, by address; Bcc is never shown.
        public var recipients: String
        /// The state word the list shows.
        public var status: String
        public var updated: Date
        /// True in the History group, where no action is offered.
        public var isHistory: Bool
        /// The raw row, for the details (spec/08 "Out Box").
        public var row: OutBoxRow

        init(_ row: OutBoxRow, isHistory: Bool, now: Date) {
            self.id = row.attemptId
            self.draftId = row.draftId
            let subject = row.subject.trimmingCharacters(in: .whitespacesAndNewlines)
            self.subject = subject.isEmpty ? OutBoxModel.noSubject : subject
            recipients = (row.recipients.filter { $0.kind == .to }
                + row.recipients.filter { $0.kind == .cc })
                .map(\.address).joined(separator: ", ")
            status = OutBoxModel.status(of: row, now: now)
            updated = Date(timeIntervalSince1970: TimeInterval(row.updatedAtMs) / 1000)
            self.isHistory = isHistory
            self.row = row
        }
    }

    /// What the user can do to one row (spec/08 "Out Box").
    public enum Action: Equatable, Sendable {
        case cancelSend
        case confirmAndSend
        case markAsSent
        case sendAgain
        case keepAsDraft
        case retryRejectedRecipients
    }

    nonisolated static let noSubject = "No Subject"

    public private(set) var rows: [Row] = []
    /// Whether any live attempt is listed, so the view polls (spec/08).
    public var hasLiveRows: Bool { rows.contains { !$0.isHistory } }
    /// Why the last reload failed, if it did; the rows keep what they
    /// showed before.
    public private(set) var lastError: (any Error)?
    /// Why the last action failed, if it did; cleared when an action
    /// starts.
    public private(set) var actionError: String?
    /// Whether an action is running now.
    public private(set) var isActing = false
    public private(set) var isRetired = false

    private let source: any SendSource
    private let now: @Sendable () -> Date
    private let canWrite: @MainActor () -> Bool
    @ObservationIgnored
    private var reloading = false
    @ObservationIgnored
    private var reloadAgain = false
    @ObservationIgnored
    private var actionSettling = false

    public init(
        source: any SendSource,
        now: @escaping @Sendable () -> Date = Date.init,
        canWrite: @escaping @MainActor () -> Bool = { true }
    ) {
        self.source = source
        self.now = now
        self.canWrite = canWrite
    }

    /// The actions `id`'s row admits (spec/08 "Out Box"); none for a
    /// history row, an id that isn't listed, or a retired model.
    public func actions(for id: String) -> [Action] {
        guard !isRetired,
              let row = rows.first(where: { $0.id == id }),
              !row.isHistory
        else { return [] }
        let attempt = row.row
        var actions: [Action] = []
        switch attempt.state {
        case .held, .queued:
            actions.append(.cancelSend)
            if attempt.state == .queued && attempt.waitsForConfirmation {
                actions.append(.confirmAndSend)
            }
        case .outcomeUnknown:
            if attempt.resolution == nil {
                actions += [.markAsSent, .sendAgain, .keepAsDraft]
            }
        case .submitting, .accepted, .failed, .cancelled:
            break
        }
        if attempt.recipients.contains(where: {
            $0.state == .rejectedPermanent || $0.state == .rejectedTransient
        }) {
            actions.append(.retryRejectedRecipients)
        }
        return actions
    }

    /// Whether the shell asks before running `action` (spec/08: Send
    /// Again's duplicate warning and Keep as Draft's).
    public static func needsConfirmation(_ action: Action) -> Bool {
        switch action {
        case .sendAgain, .keepAsDraft: return true
        case .cancelSend, .confirmAndSend, .markAsSent, .retryRejectedRecipients: return false
        }
    }

    /// Run one action on `id` under a fresh key, then reload. A refusal
    /// (retired, acting, writes unavailable, or a row that doesn't admit
    /// it) does nothing; a thrown failure shows its message.
    public func perform(_ action: Action, on id: String) async {
        guard !isRetired, !isActing, canWrite(), actions(for: id).contains(action) else { return }
        isActing = true
        actionError = nil
        let key = UUID().uuidString
        do {
            switch action {
            case .cancelSend:
                _ = try await source.cancelSend(idempotencyKey: key, attemptId: id)
            case .confirmAndSend:
                _ = try await source.confirmSendRoute(idempotencyKey: key, attemptId: id)
            case .markAsSent:
                try await resolve(id, key, .markedSent)
            case .sendAgain:
                try await resolve(id, key, .sendAgain)
            case .keepAsDraft:
                try await resolve(id, key, .keptAsDraft)
            case .retryRejectedRecipients:
                _ = try await source.retryRejectedRecipients(idempotencyKey: key, attemptId: id)
            }
        } catch {
            if !isRetired {
                actionError = (error as? CoreFailure)?.errorDescription
                    ?? error.localizedDescription
            }
        }
        // The rows still show the pre-action state until the reload
        // lands, so the action holds the model until then; a reload
        // already running reads once more and releases it.
        actionSettling = true
        await reload()
    }

    private func resolve(
        _ attemptId: String, _ key: String, _ resolution: UnknownResolution
    ) async throws {
        _ = try await source.resolveUnknownSend(
            idempotencyKey: key, attemptId: attemptId, resolution: resolution)
    }

    /// Re-read the Out Box. A call during a reload makes it run once
    /// more when it ends, and returns at once.
    public func reload() async {
        guard !isRetired else {
            settleAction()
            return
        }
        guard !reloading else {
            reloadAgain = true
            return
        }
        reloading = true
        defer {
            reloading = false
            settleAction()
        }
        repeat {
            reloadAgain = false
            await readOnce()
        } while reloadAgain && !isRetired
    }

    /// End the action whose reload just landed.
    private func settleAction() {
        guard actionSettling else { return }
        actionSettling = false
        isActing = false
    }

    /// The old archive is going away (spec/08 "Out Box").
    public func retire() {
        isRetired = true
        rows = []
        lastError = nil
        actionError = nil
    }

    private func readOnce() async {
        do {
            let box = try await source.outBox()
            guard !isRetired else { return }
            let stamp = now()
            rows = box.live.map { Row($0, isHistory: false, now: stamp) }
                + box.history.map { Row($0, isHistory: true, now: stamp) }
            lastError = nil
        } catch {
            guard !isRetired else { return }
            lastError = error
        }
    }

    nonisolated private static func status(of row: OutBoxRow, now: Date) -> String {
        switch row.state {
        case .held:
            let left = Double(row.holdUntilMs ?? 0) / 1000 - now.timeIntervalSince1970
            return "Sending in \(max(Int(left.rounded(.up)), 0)) s"
        case .queued:
            return row.waitsForConfirmation
                ? "Account changed; confirm sending with the new settings"
                : "Queued"
        case .submitting:
            return "Sending"
        case .outcomeUnknown:
            switch row.resolution {
            case nil: return "Outcome unknown"
            case .markedSent: return "Marked as sent"
            case .sendAgain: return "Sent again"
            case .keptAsDraft: return "Kept as draft"
            }
        case .accepted: return "Sent"
        case .failed: return "Failed"
        case .cancelled: return "Cancelled"
        }
    }
}
