import AppKit
import SweetbriarBridge

protocol ComposeSource: DraftStore, RecipientSource, ComposeCatalogSource, AppearanceSource, ComposeCueSource
{
    func createDraft(idempotencyKey: String, source: DraftSource) async throws -> DraftCreated
}

extension CoreBridge: ComposeSource {}

/// App-owned opens and windows outlive the action that requested them. A
/// pending create or open holds the same restore admission as the window it
/// installs. There is one window per draft (spec/08 "Drafts (local)").
@MainActor
final class ComposeWindows {
    private let backups: SharedBackups
    private let appearance: SharedAppearance
    private let cues: SharedComposeCue
    private let openCore: @MainActor () async throws -> any ComposeSource
    /// What failed, for the error's title.
    enum Action {
        case create, open
    }

    private let reportError: @MainActor (Action, any Error) -> Void
    private var pending: [UUID: Task<Void, Never>] = [:]
    private(set) var windows: [String: ComposeWindowController] = [:]
    /// Stored drafts being opened, by draft id.
    private var opening: [String: Task<ComposeWindowController?, Never>] = [:]
    /// A compose window opened or closed, or an open found its draft gone:
    /// the Drafts list reloads.
    var onWindowsChanged: (@MainActor () -> Void)?
    private var quitting: Task<Bool, Never>?
    private var isQuitting = false

    init(
        backups: SharedBackups, appearance: SharedAppearance, cues: SharedComposeCue,
        openCore: @escaping @MainActor () async throws -> any ComposeSource,
        reportError: @escaping @MainActor (Action, any Error) -> Void
    ) {
        self.backups = backups
        self.appearance = appearance
        self.cues = cues
        self.openCore = openCore
        self.reportError = reportError
    }

    func newMessage(preferredAccountId: String? = nil) {
        create(source: .new, preferredAccountId: preferredAccountId)
    }

    /// A reply, reply-all or forward draft from the message list's
    /// single selection (spec/08 "Cross-account check"). The core
    /// writes the draft's first revision, so the installed window opens
    /// on that stored revision like any draft (spec/05).
    func compose(source: DraftSource) {
        create(source: source, preferredAccountId: nil)
    }

    /// Create a draft and install its window under one admission: the
    /// same restore fence, cue check, error report and quit handling
    /// for a new message and a reply or forward.
    private func create(source: DraftSource, preferredAccountId: String?) {
        guard !isQuitting, backups.beginCompose() else { return }
        let request = UUID()
        pending[request] = Task { [self] in
            defer { pending[request] = nil }
            do {
                let core = try await openCore()
                // Restore is excluded by compose admission. A refused cue means
                // the shared context is fenced or mismatched; do not create an
                // orphan draft or a window that cannot ever follow its cue.
                guard let cue = cues.model(source: core) else {
                    throw CoreFailure.unavailable(
                        reason: "Compose appearance is unavailable. Try again after the current operation.")
                }
                let created = try await core.createDraft(
                    idempotencyKey: request.uuidString, source: source)
                install(
                    created.draftId, source: core, cue: cue,
                    preferredAccountId: preferredAccountId)
            } catch {
                backups.endCompose()
                if !isQuitting { reportError(.create, error) }
            }
        }
    }

    /// Open a stored draft from the Drafts list. Its window comes forward if
    /// it is open or opening; a failure is reported.
    func open(draftId: String) {
        if let controller = windows[draftId] {
            controller.showWindow(nil)
            return
        }
        guard opening[draftId] == nil else { return }
        _ = openStored(draftId, reportsErrors: true)
    }

    /// AppKit restoration (spec/08 "Windows"): the same admission and
    /// one-window rule as `open`, but a refusal restores nothing and
    /// reports nothing.
    func restore(draftId: String) async -> NSWindow? {
        if let controller = windows[draftId] { return controller.window }
        if let task = opening[draftId] { return await task.value?.window }
        return await openStored(draftId, reportsErrors: false)?.value?.window
    }

    private func openStored(
        _ draftId: String, reportsErrors: Bool
    ) -> Task<ComposeWindowController?, Never>? {
        guard !isQuitting, backups.beginCompose() else { return nil }
        let task = Task { [self] () -> ComposeWindowController? in
            defer { opening[draftId] = nil }
            do {
                let source = try await openCore()
                guard let cue = cues.model(source: source) else {
                    throw CoreFailure.unavailable(
                        reason: "Compose appearance is unavailable. Try again after the current operation.")
                }
                let draft = try await source.draft(id: draftId)
                guard draft.state == .editing || draft.state == .queued else {
                    throw CoreFailure.notFound(what: "the draft was sent or discarded")
                }
                guard !isQuitting else {
                    backups.endCompose()
                    return nil
                }
                return install(draftId, source: source, cue: cue, preferredAccountId: nil)
            } catch {
                backups.endCompose()
                if reportsErrors && !isQuitting { reportError(.open, error) }
                onWindowsChanged?()
                return nil
            }
        }
        opening[draftId] = task
        return task
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
            install(created.draftId, source: source, cue: cue, preferredAccountId: nil)
        } catch {
            backups.endCompose()
            throw error
        }
    }

    /// Install the draft's window, or bring its open one forward: either
    /// way the caller's admission is settled exactly once.
    @discardableResult
    private func install(
        _ draftId: String, source: any ComposeSource, cue: ComposeCueModel, preferredAccountId: String?
    ) -> ComposeWindowController {
        if let open = windows[draftId] {
            backups.endCompose()
            if !isQuitting { open.showWindow(nil) }
            return open
        }
        let controller = ComposeWindowController(
            source: source, draftId: draftId, preferredAccountId: preferredAccountId,
            appearance: appearance.model(source: source), cue: cue,
            canWrite: { [backups] in backups.canWrite },
            openCopy: { [weak self] in
                guard let self else { throw CoreFailure.unavailable(reason: "Sweetbriar is closing.") }
                try await openCopy(source: $0, content: $1, createKey: $2, saveKey: $3)
            },
            sendSource: source as? any SendSource,
            settingsSource: source as? any SendSafetySettingsSource)
        windows[draftId] = controller
        controller.onClosed = { [weak self] in
            guard let self else { return }
            windows[draftId] = nil
            backups.endCompose()
            onWindowsChanged?()
        }
        // Quit may have arrived during create. Finish ownership, but
        // do not bring a late window in front of the quit decision.
        if !isQuitting { controller.showWindow(nil) }
        onWindowsChanged?()
        return controller
    }

    /// Called before Backups/Core shutdown. A failure leaves the owner usable
    /// for an explicit retry; no shutdown happens while unsaved text remains.
    func closeForQuit() async -> Bool {
        if let quitting { return await quitting.value }
        isQuitting = true
        let opens = Array(pending.values)
        let stored = Array(opening.values)
        let task = Task { [self] in
            for open in opens { await open.value }
            for open in stored { _ = await open.value }
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
