import AppKit
import SweetbriarBridge

/// Owns native selection, signature sheets and the route half of text undo.
/// A proposed body is accepted synchronously before DraftModel publishes it.
@MainActor
final class ComposeIdentityControls: NSObject {
    enum Event { case presentation, edit, decision }
    let popup = NSPopUpButton()
    let refreshButton = NSButton(title: "Refresh Identities", target: nil, action: nil)
    private(set) lazy var model: ComposeIdentityModel = {
        let model = ComposeIdentityModel(
            draft: draft, source: catalogSource, preferredAccountId: preferredAccountId,
            canApply: { [weak self] in self?.admit($0) ?? false })
        model.onTransition = { [weak self] _ in self?.changed(.edit) }
        return model
    }()
    private(set) var isPublishing = false
    private(set) var problem: String?
    var isDeciding: Bool { model.pendingDecision != nil }
    var titleContext: String? {
        guard let identity = model.selectedIdentity else { return nil }
        return "\(identity.accountName) · \(identity.email)"
    }
    private let draft: DraftModel
    private let catalogSource: any ComposeCatalogSource
    private let preferredAccountId: String?
    private let textView: ComposerTextView
    private weak var window: NSWindow?
    private let editable: @MainActor () -> Bool
    private let inputReady: @MainActor () -> Bool
    private let prepareInput: @MainActor () async -> Bool
    private let changed: @MainActor (Event) -> Void
    private var discovery: Task<Void, Never>?
    private var selection: Task<Void, Never>?
    private var ticket: UInt64 = 0
    private var stopped = false
    private var alert: NSAlert?

    init(
        draft: DraftModel, source: any ComposeCatalogSource, preferredAccountId: String?,
        textView: ComposerTextView, window: NSWindow,
        editable: @escaping @MainActor () -> Bool, inputReady: @escaping @MainActor () -> Bool,
        prepareInput: @escaping @MainActor () async -> Bool,
        changed: @escaping @MainActor (Event) -> Void
    ) {
        self.draft = draft
        catalogSource = source
        self.preferredAccountId = preferredAccountId
        self.textView = textView
        self.window = window
        self.editable = editable
        self.inputReady = inputReady
        self.prepareInput = prepareInput
        self.changed = changed
        super.init()
        refreshButton.target = self
        refreshButton.action = #selector(refresh)
        refreshButton.setAccessibilityIdentifier("composeRefreshIdentities")
        popup.target = self
        popup.action = #selector(selected)
        popup.setAccessibilityLabel("From")
        popup.setAccessibilityIdentifier("composeFrom")
    }

    @objc private func refresh() { startDiscovery() }

    /// Draft loading must finish first; defaults must never beat its saved route.
    func startDiscovery() {
        guard !stopped, draft.content != nil, discovery == nil else { return }
        discovery = Task { [weak self] in
            guard let self, !stopped, !Task.isCancelled else { return }
            await model.reload()
            discovery = nil
            changed(.presentation)
        }
        changed(.presentation)
    }

    @objc private func selected() {
        guard let item = popup.selectedItem else { return }
        if let id = item.representedObject as? String {
            choose(identityId: id)
        } else if item.representedObject is NSNull {
            choose(identityId: nil)
        }
    }

    func choose(identityId: String?) {
        guard !stopped, editable(), !isDeciding else { return }
        ticket &+= 1
        let current = ticket
        let previous = selection
        previous?.cancel()
        render()  // Don't display a proposed identity as the saved/current route.
        selection = Task { [weak self] in
            await previous?.value
            guard let self, !stopped, current == ticket, !Task.isCancelled else { return }
            let ready = await prepareInput()
            guard !stopped, current == ticket, !Task.isCancelled, editable() else { return }
            guard ready else {
                problem = "Finish the current input before changing From."
                changed(.presentation)
                return
            }
            problem = nil
            let result = model.choose(identityId: identityId)
            if result == .needsSignatureChoice { showSignatureChoice() }
            changed(.presentation)
        }
    }

    private func showSignatureChoice() {
        guard let decision = model.pendingDecision, let window, window.attachedSheet == nil else {
            model.cancelDecision()
            problem = "Close the current sheet and choose From again."
            return
        }
        let alert = NSAlert()
        alert.messageText = "Keep your text or append the signature?"
        alert.informativeText =
            "From will change to \(decision.identity.email). Your current text is not an unchanged signature inserted by this editor."
        alert.addButton(withTitle: "Keep Text")
        alert.addButton(withTitle: "Append Signature")
        self.alert = alert
        changed(.decision)
        alert.beginSheetModal(for: window) { [weak self] response in
            guard let self, self.alert === alert else { return }
            self.alert = nil
            if response == .alertFirstButtonReturn || response == .alertSecondButtonReturn {
                _ = model.resolve(
                    decisionId: decision.id,
                    choice: response == .alertFirstButtonReturn ? .keepText : .append)
            } else {
                model.cancelDecision()
            }
            changed(.decision)
        }
    }

    func cancelDecision() {
        ticket &+= 1
        selection?.cancel()
        guard isDeciding || alert != nil else { return }
        model.cancelDecision()
        if let alert {
            self.alert = nil
            window?.endSheet(alert.window, returnCode: .abort)
            alert.window.orderOut(nil)
        }
        changed(.decision)
    }

    private func admit(_ transition: ComposeIdentityModel.Transition) -> Bool {
        guard !stopped, editable(), textView.isEditable, !textView.hasMarkedText(),
            let undo = textView.undoManager
        else { return false }
        // TextKit owns body restoration in this group. Its delegate notification
        // can follow the route action, so don't reject a route-only redo because
        // the model has not captured that native body change yet.
        if undo.isUndoing || undo.isRedoing {
            return transition.previousBody.utf8.elementsEqual(transition.body.utf8)
        }
        guard inputReady(), textView.string.utf8.elementsEqual(transition.previousBody.utf8) else {
            return false
        }
        isPublishing = true
        defer { isPublishing = false }
        textView.breakUndoCoalescing()
        undo.beginUndoGrouping()
        defer {
            undo.endUndoGrouping()
            textView.breakUndoCoalescing()
        }
        if !transition.previousBody.utf8.elementsEqual(transition.body.utf8),
            !textView.replaceSourceUndoably(transition.body)
        {
            return false
        }
        recordRouteUndo(
            transition.previousIdentityId, reciprocal: transition.identityId, using: undo)
        undo.setActionName("Change From")
        return true
    }

    private func recordRouteUndo(_ id: String?, reciprocal: String?, using undo: UndoManager) {
        undo.registerUndo(withTarget: self) { owner in
            MainActor.assumeIsolated { owner.restoreRoute(id, reciprocal: reciprocal, using: undo) }
        }
    }

    private func restoreRoute(_ id: String?, reciprocal: String?, using undo: UndoManager) {
        guard !stopped, model.restoreRoute(identityId: id) else { return }
        recordRouteUndo(reciprocal, reciprocal: id, using: undo)
        changed(.edit)
    }

    func render() {
        let selected = draft.content?.identityId
        popup.removeAllItems()
        popup.addItem(withTitle: "Without a sending identity")
        popup.lastItem?.representedObject = NSNull()
        var selectedIndex = 0
        for identity in model.choices {
            let provisional = identity.sendable == .provisional ? " (provisional)" : ""
            let name = identity.displayName.isEmpty ? "" : identity.displayName + " "
            popup.addItem(
                withTitle: "\(name)<\(identity.email)> · \(identity.accountName)\(provisional)")
            popup.lastItem?.representedObject = identity.identityId
            if selected?.utf8.elementsEqual(identity.identityId.utf8) == true {
                selectedIndex = popup.numberOfItems - 1
            }
        }
        if selected != nil, selectedIndex == 0 {
            popup.addItem(
                withTitle: "Unavailable identity: \(model.selectedIdentity?.email ?? selected ?? "")")
            popup.lastItem?.isEnabled = false
            selectedIndex = popup.numberOfItems - 1
        }
        popup.selectItem(at: selectedIndex)
        popup.isEnabled = !stopped && editable() && model.isReady && !isDeciding
        refreshButton.isEnabled = !stopped && editable() && discovery == nil && !isDeciding
        popup.toolTip = model.problem ?? problem
    }

    func stopAndWait() async {
        stopped = true
        ticket &+= 1
        cancelDecision()
        discovery?.cancel()
        selection?.cancel()
        await model.stopAndWait()
        await discovery?.value
        await selection?.value
        textView.undoManager?.removeAllActions(withTarget: self)
        popup.target = nil
        refreshButton.target = nil
    }
}
