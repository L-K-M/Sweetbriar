// Sending in the compose window across the L1 boundary (spec/05
// "Outbox state machine", spec/08 "Sending"): settle the pending save,
// queue the current revision under a fresh key, hold the attempt for the
// registered undo window, and cancel it on Undo. The window's own close
// stays in the shell. A reply's cross-account confirmation comes with
// the banner in M4.4d: until then its `Conflict` shows as any other
// failure.

import Foundation
import Observation

/// One compose window's send flow over `SendSource` (spec/05, 07, 08).
/// The window supplies its pending-save settle and its close; tests
/// substitute fakes and a controllable sleeper.
@MainActor
@Observable
public final class ComposeSendModel {
    public enum State: Equatable {
        case idle
        case sending
        case held(attemptId: String, remaining: Int)
        case failed(message: String)
    }

    /// Settle the window's pending save the way Save/Close already do;
    /// answer the current saved revision id, nil when nothing is saved.
    public typealias SettlePendingSave = @MainActor () async throws -> String?
    /// Close the compose window after the hold ends without Undo.
    public typealias CloseAfterHold = @MainActor () -> Void

    public private(set) var state: State = .idle

    private let source: any SendSource
    private let settingsSource: any SendSafetySettingsSource
    private let draftId: String
    private let settlePendingSave: SettlePendingSave
    private let closeAfterHold: CloseAfterHold
    private let sleep: @Sendable (Duration) async throws -> Void

    /// The action's key and revision: a retry of the same action replays
    /// the key, a new saved revision starts a new action (spec/07).
    private var sendKey: String?
    private var sendRevision: String?
    private var cancelKey: String?
    private var cancelAttemptId: String?
    private var holdTask: Task<Void, Never>?

    public init(
        source: any SendSource,
        settingsSource: any SendSafetySettingsSource,
        draftId: String,
        settlePendingSave: @escaping SettlePendingSave,
        closeAfterHold: @escaping CloseAfterHold = {},
        sleep: @escaping @Sendable (Duration) async throws -> Void = { try await Task.sleep(for: $0) }
    ) {
        self.source = source
        self.settingsSource = settingsSource
        self.draftId = draftId
        self.settlePendingSave = settlePendingSave
        self.closeAfterHold = closeAfterHold
        self.sleep = sleep
    }

    /// True while a send or its cancellation owns the window: the editor
    /// stays frozen and the Send button disabled.
    public var isBusy: Bool {
        switch state {
        case .sending, .held: return true
        case .idle, .failed: return false
        }
    }

    /// Settle, queue and hold the current saved revision. A failure
    /// keeps the draft editable and carries its message.
    @discardableResult
    public func send() async -> Bool {
        guard !isBusy else { return false }
        state = .sending
        let revision: String
        do {
            guard let settled = try await settlePendingSave() else {
                throw CoreFailure.unavailable(reason: "Save this draft before sending it.")
            }
            revision = settled
        } catch {
            fail(with: error)
            return false
        }
        if sendRevision != revision {
            sendRevision = revision
            sendKey = UUID().uuidString
        }
        guard let key = sendKey else {
            fail(with: CoreFailure.internalError(detail: "the send action has no idempotency key"))
            return false
        }
        do {
            let queued = try await source.sendDraft(
                idempotencyKey: key, draftId: draftId, revisionId: revision, acks: [])
            await beginHold(queued.attemptId)
            return true
        } catch {
            fail(with: error)
            return false
        }
    }

    /// Cancel a held attempt exactly once. Success returns the draft to
    /// editing; a failure (a hold that already ended, say) shows its
    /// message and still leaves the draft editable.
    @discardableResult
    public func undo() async -> Bool {
        guard case .held(let attemptId, _) = state else { return false }
        holdTask?.cancel()
        holdTask = nil
        if cancelAttemptId != attemptId {
            cancelAttemptId = attemptId
            cancelKey = UUID().uuidString
        }
        guard let key = cancelKey else {
            state = .idle
            return false
        }
        // Freeze only while the cancellation is in flight; its outcome
        // decides whether the editor returns.
        state = .sending
        do {
            _ = try await source.cancelSend(idempotencyKey: key, attemptId: attemptId)
            state = .idle
            clearAction()
            return true
        } catch {
            fail(with: error)
            return false
        }
    }

    private func beginHold(_ attemptId: String) async {
        let seconds = await currentUndoSeconds()
        let sleep = sleep
        let task = Task { [weak self] in
            var remaining = seconds
            while remaining > 0 {
                guard !Task.isCancelled else { return }
                self?.state = .held(attemptId: attemptId, remaining: remaining)
                do {
                    try await sleep(.seconds(1))
                } catch {
                    return
                }
                remaining -= 1
            }
            guard !Task.isCancelled, let self else { return }
            self.state = .idle
            self.clearAction()
            self.closeAfterHold()
        }
        holdTask = task
        state = .held(attemptId: attemptId, remaining: seconds)
    }

    /// The window mirrors the attempt's hold from the setting read at
    /// send time. Only a failed read falls back to the registry default;
    /// a valid stored choice always wins.
    private func currentUndoSeconds() async -> Int {
        if let settings = try? await settingsSource.settings(),
            let seconds = SendSafety.undoSeconds(in: settings)
        {
            return seconds
        }
        return SendSafety.fallbackUndoSeconds
    }

    private func clearAction() {
        sendKey = nil
        sendRevision = nil
        cancelKey = nil
        cancelAttemptId = nil
    }

    private func fail(with error: any Error) {
        state = .failed(
            message: (error as? CoreFailure)?.errorDescription ?? error.localizedDescription)
    }
}
