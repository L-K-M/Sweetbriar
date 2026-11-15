import Foundation
import SweetbriarBridge

protocol ComposeSource: DraftStore, RecipientSource, ComposeCatalogSource, AppearanceSource, ComposeCueSource
{
    func createDraft(idempotencyKey: String, source: DraftSource) async throws -> DraftCreated
}

extension CoreBridge: ComposeSource {}

/// App-owned opens and windows outlive the action that requested them. A
/// pending create holds the same restore admission as the window it installs.
@MainActor
final class ComposeWindows {
    private let backups: SharedBackups
    private let appearance: SharedAppearance
    private let cues: SharedComposeCue
    private let openCore: @MainActor () async throws -> any ComposeSource
    private let reportError: @MainActor (any Error) -> Void
    private var pending: [UUID: Task<Void, Never>] = [:]
    private(set) var windows: [String: ComposeWindowController] = [:]
    private var quitting: Task<Bool, Never>?
    private var isQuitting = false

    init(
        backups: SharedBackups, appearance: SharedAppearance, cues: SharedComposeCue,
        openCore: @escaping @MainActor () async throws -> any ComposeSource,
        reportError: @escaping @MainActor (any Error) -> Void
    ) {
        self.backups = backups
        self.appearance = appearance
        self.cues = cues
        self.openCore = openCore
        self.reportError = reportError
    }

    func newMessage(preferredAccountId: String? = nil) {
        guard !isQuitting, backups.beginCompose() else { return }
        let request = UUID()
        pending[request] = Task { [self] in
            defer { pending[request] = nil }
            do {
                let source = try await openCore()
                guard let cue = cues.model(source: source) else {
                    throw CoreFailure.unavailable(
                        reason: "Compose appearance is unavailable. Try again after the current operation.")
                }
                let created = try await source.createDraft(idempotencyKey: request.uuidString, source: .new)
                let controller = ComposeWindowController(
                    source: source, draftId: created.draftId, preferredAccountId: preferredAccountId,
                    appearance: appearance.model(source: source), cue: cue,
                    canWrite: { [backups] in backups.canWrite })
                precondition(windows[created.draftId] == nil, "a fresh create key returns a distinct draft")
                windows[created.draftId] = controller
                controller.onClosed = { [weak self] in
                    guard let self else { return }
                    windows[created.draftId] = nil
                    backups.endCompose()
                }
                // Quit may have arrived during create. Finish ownership, but
                // do not bring a late window in front of the quit decision.
                if !isQuitting { controller.showWindow(nil) }
            } catch {
                backups.endCompose()
                if !isQuitting { reportError(error) }
            }
        }
    }

    /// Called before Backups/Core shutdown. A failure leaves the owner usable
    /// for an explicit retry; no shutdown happens while unsaved text remains.
    func closeForQuit() async -> Bool {
        if let quitting { return await quitting.value }
        isQuitting = true
        let opens = Array(pending.values)
        let task = Task { [self] in
            for open in opens { await open.value }
            var allClosed = true
            for controller in Array(windows.values) {
                if !(await controller.closeAndWait()) {
                    allClosed = false
                    controller.showWindow(nil)
                }
            }
            if !allClosed { isQuitting = false }
            return allClosed
        }
        quitting = task
        let result = await task.value
        if !result { quitting = nil }
        return result
    }
}
