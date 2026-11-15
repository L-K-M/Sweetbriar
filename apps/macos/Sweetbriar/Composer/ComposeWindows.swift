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
                // Restore is excluded by compose admission. A refused cue means
                // the shared context is fenced or mismatched; do not create an
                // orphan draft or a window that cannot ever follow its cue.
                guard let cue = cues.model(source: source) else {
                    throw CoreFailure.unavailable(
                        reason: "Compose appearance is unavailable. Try again after the current operation.")
                }
                let created = try await source.createDraft(idempotencyKey: request.uuidString, source: .new)
                install(created.draftId, source: source, cue: cue, preferredAccountId: preferredAccountId)
            } catch {
                backups.endCompose()
                if !isQuitting { reportError(error) }
            }
        }
    }

    /// Keep Both (spec/08 "Saving"): create the copy, save the conflicted
    /// window's content as its first revision, then open it. The caller
    /// keeps both keys for an explicit retry, so a lost receipt never makes
    /// a second copy. Nothing here retries by itself.
    func openCopy(
        source copySource: DraftSource, content: DraftContent, createKey: String, saveKey: String
    ) async throws {
        guard !isQuitting, backups.beginCompose() else {
            throw CoreFailure.unavailable(reason: "Drafts can't be copied during quit or restore.")
        }
        do {
            let source = try await openCore()
            guard let cue = cues.model(source: source) else {
                throw CoreFailure.unavailable(
                    reason: "Compose appearance is unavailable. Try again after the current operation.")
            }
            let created = try await source.createDraft(idempotencyKey: createKey, source: copySource)
            let base = try await source.draft(id: created.draftId).revision?.revisionId
            _ = try await source.saveDraft(
                idempotencyKey: saveKey, draftId: created.draftId, baseRevisionId: base, content: content)
            guard windows[created.draftId] == nil else {
                backups.endCompose()
                return
            }
            install(created.draftId, source: source, cue: cue, preferredAccountId: nil)
        } catch {
            backups.endCompose()
            throw error
        }
    }

    private func install(
        _ draftId: String, source: any ComposeSource, cue: ComposeCueModel, preferredAccountId: String?
    ) {
        let controller = ComposeWindowController(
            source: source, draftId: draftId, preferredAccountId: preferredAccountId,
            appearance: appearance.model(source: source), cue: cue,
            canWrite: { [backups] in backups.canWrite },
            openCopy: { [weak self] in
                guard let self else { throw CoreFailure.unavailable(reason: "Sweetbriar is closing.") }
                try await openCopy(source: $0, content: $1, createKey: $2, saveKey: $3)
            })
        precondition(windows[draftId] == nil, "a fresh create key returns a distinct draft")
        windows[draftId] = controller
        controller.onClosed = { [weak self] in
            guard let self else { return }
            windows[draftId] = nil
            backups.endCompose()
        }
        // Quit may have arrived during create. Finish ownership, but
        // do not bring a late window in front of the quit decision.
        if !isQuitting { controller.showWindow(nil) }
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
