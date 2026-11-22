// One main window's filing (spec/08 "Keyboard filing"): admits the
// commands, captures the list's selection, opens the palette, and
// shows the toast, its Undo and the refusals. `FilingModel` runs them.

import AppKit
import Observation
import SweetbriarBridge

@MainActor
final class FilingController: NSObject {
    let model: FilingModel
    let toastView = FilingToastView()
    private(set) var palette: FilingPalette?

    private weak var window: NSWindow?
    private let list: MessageListViewController
    private let sidebar: SidebarModel
    private let appearance: AppearanceModel
    private let writes: ArchiveWriteTasks
    /// The archive takes writes and the list pane shows messages.
    private let canFile: @MainActor () -> Bool
    private let canWrite: @MainActor () -> Bool
    private var offered: FilingToast.ID?
    private var retired = false

    init(
        source: any FilingSource, window: NSWindow, list: MessageListViewController,
        sidebar: SidebarModel, appearance: AppearanceModel, writes: ArchiveWriteTasks,
        canWrite: @escaping @MainActor () -> Bool, canFile: @escaping @MainActor () -> Bool
    ) {
        model = FilingModel(source: source)
        self.window = window
        self.list = list
        self.sidebar = sidebar
        self.appearance = appearance
        self.writes = writes
        self.canWrite = canWrite
        self.canFile = canFile
        super.init()
        toastView.onUndo = { [weak self] id in self?.undo(id) }
        observeToast()
    }

    /// The commands act in the key main window on a selection, never
    /// while text is being edited.
    var isEnabled: Bool {
        list.list.map { canAct(on: $0.selectedIDs) } ?? false
    }

    /// The commands' rule for `ids`: the selection, or the rows a
    /// context click targets.
    func canAct(on ids: Set<Int64>) -> Bool {
        guard !ids.isEmpty, !retired, let window, window.isKeyWindow else { return false }
        return window.attachedSheet == nil && !window.isEditingText && canFile()
    }

    /// Archive or Move to Trash, on `ids` or the selection.
    func file(_ action: FilingAction, ids: Set<Int64>? = nil) {
        guard let captured = capture(ids), canAct(on: captured.ids) else {
            NSSound.beep()
            return
        }
        writes.run(revision: writes.revision) { [weak self] in
            guard let self, let selection = await resolve(captured) else { return }
            await perform(action, on: selection)
        }
    }

    /// Move to… or a label command: the palette, then the action on
    /// the selection, or `ids`, as it was when the command was invoked.
    func choose(_ choice: FilingChoice, ids: Set<Int64>? = nil) {
        guard let captured = capture(ids), canAct(on: captured.ids) else {
            NSSound.beep()
            return
        }
        let revision = writes.revision
        writes.run(revision: revision) { [weak self] in
            guard let self, let selection = await resolve(captured) else { return }
            await present(
                choice, for: selection, shownMailboxId: captured.model.mailboxId, revision: revision)
        }
    }

    /// Undo, from the toast or Edit → Undo.
    func undo(_ id: FilingToast.ID) {
        guard !retired, canWrite() else {
            NSSound.beep()
            return
        }
        writes.run(revision: writes.revision) { [weak self] in
            guard let self else { return }
            let failures = await model.undo(id)
            explain(failures, title: "Couldn't undo", accountsNamed: false)
        }
    }

    /// The archive's content is being retired: nothing from it is
    /// offered again.
    func retire() {
        retired = true
        model.retire()
        toastView.show(nil)
        window?.undoManager?.removeAllActions(withTarget: self)
        palette?.cancel(nil)
    }

    private func capture(_ ids: Set<Int64>?) -> (model: ListWindowModel, ids: Set<Int64>)? {
        guard let model = list.model, let ids = ids ?? list.list?.selectedIDs else { return nil }
        return (model, ids)
    }

    private func resolve(
        _ captured: (model: ListWindowModel, ids: Set<Int64>)
    ) async -> FilingSelection? {
        do {
            return try await captured.model.filingSelection(of: captured.ids)
        } catch is CancellationError {
            return nil
        } catch {
            explain([FilingFailure(error: error)], title: Self.refusedTitle, accountsNamed: false)
            return nil
        }
    }

    private func present(
        _ choice: FilingChoice, for selection: FilingSelection, shownMailboxId: Int64?,
        revision: UInt64
    ) async {
        guard let accountId = selection.onlyAccountId else {
            explain(
                [FilingFailure(error: FilingRefusal.severalAccounts)],
                title: Self.refusedTitle, accountsNamed: false)
            return
        }
        guard let mailboxes = sidebar.mailboxes[accountId] else {
            explain(
                [FilingFailure(error: FilingRefusal.mailboxesUnread)],
                title: Self.refusedTitle, accountsNamed: false)
            return
        }
        let candidates = choice.candidates(in: mailboxes, shownMailboxId: shownMailboxId)
        let name = accountName(accountId)
        guard !candidates.isEmpty else {
            showExplanation(choice.nothingToChoose(account: name), title: Self.refusedTitle)
            return
        }

        await model.loadRecentMailboxes()
        guard !retired, !Task.isCancelled, let window, window.isVisible, window.attachedSheet == nil
        else { return }
        let palette = FilingPalette(
            choice: choice, mailboxes: candidates, recent: model.recentMailboxIds,
            accountName: name,
            account: appearance.appearance?.accounts.first { $0.accountId == accountId })
        self.palette = palette
        palette.begin(on: window) { [weak self] mailbox in
            guard let self else { return }
            self.palette = nil
            list.list?.focus()
            guard !retired, let mailbox, canWrite() else { return }
            // A restore admitted meanwhile refuses this revision.
            writes.run(revision: revision) { [weak self] in
                await self?.perform(choice.action(mailbox), on: selection)
            }
        }
    }

    fileprivate static let refusedTitle = "Couldn't file the messages"

    private func perform(_ action: FilingAction, on selection: FilingSelection) async {
        let failures = await model.perform(action, on: selection, mailboxes: sidebar.mailboxes)
        let partly = failures.count < selection.parts.count
        explain(
            failures, title: partly ? "Some messages weren't filed" : Self.refusedTitle,
            accountsNamed: selection.parts.count > 1)
    }

    private func accountName(_ id: String) -> String {
        guard let section = sidebar.sections.first(where: { $0.id == id }) else { return id }
        return SidebarViewController.accountLabel(section, among: sidebar.sections)
    }

    private func explain(_ failures: [FilingFailure], title: String, accountsNamed: Bool) {
        guard !failures.isEmpty else { return }
        let lines = failures.map { failure in
            guard accountsNamed, let account = failure.accountId else { return failure.explanation }
            return "\(accountName(account)): \(failure.explanation)"
        }
        showExplanation(lines.joined(separator: "\n"), title: title)
    }

    private func showExplanation(_ text: String, title: String) {
        guard !retired, let window, window.isVisible else { return }
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = text
        alert.beginSheetModal(for: window) { _ in }
    }

    private func observeToast() {
        withObservationTracking {
            _ = model.toast
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, !retired else { return }
                toastDidChange()
                observeToast()
            }
        }
    }

    /// Show the toast, announce a new one, and keep Edit → Undo to it.
    private func toastDidChange() {
        let toast = model.toast
        toastView.show(toast)
        guard toast?.id != offered else { return }
        offered = toast?.id
        let undoManager = window?.undoManager
        undoManager?.removeAllActions(withTarget: self)
        guard let toast else { return }

        NSAccessibility.post(
            element: NSApplication.shared, notification: .announcementRequested,
            userInfo: [
                .announcement: toast.message,
                .priority: NSAccessibilityPriorityLevel.high.rawValue,
            ])
        guard toast.canUndo, let undoManager else { return }
        let id = toast.id
        undoManager.registerUndo(withTarget: self) { controller in
            MainActor.assumeIsolated { controller.undo(id) }
        }
        undoManager.setActionName("Filing")
    }
}

// MARK: - Drops on the sidebar

extension FilingController: MessageDropTarget {
    func takesDrop(from source: Any?) -> Bool {
        guard !retired, canFile(), let window, window.attachedSheet == nil else { return false }
        return list.list?.isDragSource(source) == true
    }

    func drop(from source: Any?, onto mailbox: Mailbox) -> Bool {
        guard takesDrop(from: source), let captured = capture(list.list?.draggedIDs),
            !captured.ids.isEmpty
        else { return false }
        return writes.run(revision: writes.revision) { [weak self] in
            guard let self, let selection = await resolve(captured) else { return }
            do {
                let action = try FilingDestinations.dropAction(
                    onto: mailbox, shownMailboxId: captured.model.mailboxId)
                await perform(action, on: selection)
            } catch {
                explain(
                    [FilingFailure(error: error)], title: Self.refusedTitle, accountsNamed: false)
            }
        }
    }
}
