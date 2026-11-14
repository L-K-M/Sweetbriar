import AppKit
import Observation
import SweetbriarBridge

/// One draft's text editor. Other revision fields remain owned by DraftModel;
/// text edits never reconstruct recipients, routing or attachment references.
@MainActor
final class ComposeWindowController: NSWindowController, NSWindowDelegate, NSTextFieldDelegate,
    NSTextViewDelegate
{
    let model: DraftModel
    let textView = ComposerTextView()
    let subjectField = NSTextField()
    let statusLabel = NSTextField(wrappingLabelWithString: "Loading draft…")
    private let modeControl = NSSegmentedControl(
        labels: ["Plain", "Markdown"], trackingMode: .selectOne,
        target: nil, action: nil)
    private let saveButton = NSButton(title: "Save", target: nil, action: nil)
    private let abandonButton = NSButton(title: "Close Without Saving…", target: nil, action: nil)
    private let autosave: DraftAutosave
    private let canWrite: @MainActor () -> Bool
    private var loadTask: Task<Void, Never>?
    private var closing: Task<Bool, Never>?
    private var closeIntent: CloseMode?
    private var abandonAlert: NSAlert?
    var isClosing: Bool { closeIntent != nil }
    private var finished = false
    var onClosed: (@MainActor () -> Void)?

    init(source: any DraftStore, draftId: String, canWrite: @escaping @MainActor () -> Bool = { true }) {
        self.canWrite = canWrite
        model = DraftModel(source: ComposeSavePolicy(source: source, canWrite: canWrite), draftId: draftId)
        autosave = DraftAutosave(model: model)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 680, height: 520),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = "New Message"
        window.isReleasedWhenClosed = false
        window.contentMinSize = NSSize(width: 600, height: 320)
        window.setAccessibilityIdentifier("compose-\(draftId)")
        super.init(window: window)
        window.delegate = self
        buildContent(window)
        render()
        observeModel()
        loadTask = Task { [self] in
            await model.load()
            render()
        }
    }

    required init?(coder: NSCoder) { fatalError("ComposeWindowController is built in code") }

    private func buildContent(_ window: NSWindow) {
        subjectField.placeholderString = "Subject"
        subjectField.setAccessibilityLabel("Subject")
        subjectField.setAccessibilityIdentifier("composeSubject")
        subjectField.delegate = self
        textView.delegate = self
        textView.setAccessibilityIdentifier("composeBody")
        modeControl.target = self
        modeControl.action = #selector(modeChanged)
        saveButton.target = self
        saveButton.action = #selector(saveNow)
        saveButton.setAccessibilityIdentifier("composeSave")
        abandonButton.target = self
        abandonButton.action = #selector(confirmAbandon)
        abandonButton.setAccessibilityIdentifier("composeCloseWithoutSaving")
        statusLabel.setAccessibilityIdentifier("composeSaveStatus")
        statusLabel.textColor = .secondaryLabelColor
        let identity = NSTextField(
            wrappingLabelWithString:
                "Local draft. Sending is not available yet. Recipient and attachment controls follow in a later step."
        )
        identity.textColor = .secondaryLabelColor
        let send = NSButton(title: "Send", target: nil, action: nil)
        send.isEnabled = false
        send.toolTip = "Sending is not available yet. You can save a local draft without a sending account."
        send.setAccessibilityIdentifier("composeSend")
        let controls = NSStackView(views: [modeControl, saveButton, send, abandonButton])
        controls.spacing = 8
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 640, height: 340))
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder
        scroll.documentView = textView
        textView.minSize = NSSize(width: 0, height: scroll.contentSize.height)
        textView.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude, height: CGFloat.greatestFiniteMagnitude)
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.textContainer?.containerSize = NSSize(
            width: scroll.contentSize.width, height: CGFloat.greatestFiniteMagnitude)
        textView.textContainer?.widthTracksTextView = true
        let stack = NSStackView(views: [identity, subjectField, controls, scroll, statusLabel])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(stack)
        window.contentView = content
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -16),
            stack.topAnchor.constraint(equalTo: content.topAnchor, constant: 16),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -12),
            identity.widthAnchor.constraint(equalTo: stack.widthAnchor),
            subjectField.widthAnchor.constraint(equalTo: stack.widthAnchor),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 120),
            statusLabel.widthAnchor.constraint(equalTo: stack.widthAnchor),
        ])
        scroll.setContentHuggingPriority(.defaultLow, for: .vertical)
        window.center()
    }

    func controlTextDidChange(_ notification: Notification) { captureEdit() }
    func textDidChange(_ notification: Notification) { captureEdit() }
    @objc private func modeChanged() { captureEdit() }

    private func captureEdit() {
        guard !finished, !isClosing, var content = model.content else { return }
        let format: ComposeFormat = modeControl.selectedSegment == 0 ? .plain : .markdown
        textView.mode = format == .plain ? .plain : .markdown
        // Canonical String equality conflates distinct source bytes. An edit
        // must retain the spelling actually entered in the native buffers.
        guard
            !content.subject.utf8.elementsEqual(subjectField.stringValue.utf8)
                || !content.bodySource.utf8.elementsEqual(textView.string.utf8)
                || content.format != format
        else { return }
        content.subject = subjectField.stringValue
        content.bodySource = textView.string
        content.format = format
        model.update(content)
        autosave.contentDidChange()
        render()
    }

    @objc func saveNow() {
        guard !isClosing, !finished, abandonAlert == nil else { return }
        captureEdit()
        Task { [self] in
            _ = await autosave.flush()
            render()
        }
    }

    func windowDidResignKey(_ notification: Notification) {
        guard !isClosing, !finished, abandonAlert == nil else { return }
        captureEdit()
        Task { [self] in
            _ = await autosave.flush()
            render()
        }
    }

    @objc private func confirmAbandon() {
        guard !isClosing, !finished, let window, window.attachedSheet == nil else { return }
        let alert = NSAlert()
        alert.messageText = "Close without saving your changes?"
        alert.informativeText =
            "Unsaved editor changes will be lost. Your last saved revision remains. Any save already in progress will finish; it is not rolled back."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Keep Editing")
        alert.addButton(withTitle: "Close Without Saving").hasDestructiveAction = true
        autosave.suspendDebounce()
        abandonAlert = alert
        alert.beginSheetModal(for: window) { [weak self] response in
            guard let self else { return }
            abandonAlert = nil
            if response == .alertSecondButtonReturn {
                _ = beginClose(mode: .abandonEdits)
            } else if !isClosing, !finished {
                autosave.contentDidChange()
            }
        }
    }

    /// A failed close leaves text and its base untouched. All callers share
    /// one close decision, and cancellation cannot abandon the owned save.
    func closeAndWait() async -> Bool { await close(mode: .save) }

    /// Only after explicit confirmation. This is not discard_draft: immutable
    /// saved revisions, including a late receipt, remain in the archive.
    func closeWithoutSavingAndWait() async -> Bool { await close(mode: .abandonEdits) }

    private enum CloseMode { case save, abandonEdits }

    private func close(mode: CloseMode) async -> Bool {
        if finished { return true }
        guard let task = beginClose(mode: mode) else { return false }
        return await task.value
    }

    /// Synchronous admission also fences a deadline in the sheet's response
    /// callback, before another main-actor task can begin a new save.
    private func beginClose(mode: CloseMode) -> Task<Bool, Never>? {
        if let closing { return closing }
        guard !finished else { return nil }
        if mode == .save {
            if let alert = abandonAlert {
                window?.endSheet(alert.window, returnCode: .alertFirstButtonReturn)
                alert.window.orderOut(nil)
                abandonAlert = nil
            }
            // Let the field editor/input method commit before freezing controls.
            guard window?.makeFirstResponder(nil) != false, !textView.hasMarkedText() else {
                statusLabel.stringValue = "Finish composing text before closing this draft."
                return nil
            }
            captureEdit()
        } else {
            // Fence an unbegun deadline before scheduling the drain task.
            autosave.stop()
        }
        closeIntent = mode
        render()
        let task = Task { [self] in
            await loadTask?.value
            if mode == .save, model.content != nil, !(await autosave.flush()) {
                closeIntent = nil
                closing = nil
                render()
                return false
            }
            await autosave.stopAndWait()
            model.stop()
            finished = true
            window?.close()
            onClosed?()
            return true
        }
        closing = task
        return task
    }

    func windowShouldClose(_ sender: NSWindow) -> Bool {
        if finished { return true }
        _ = beginClose(mode: .save)
        return false
    }

    private func observeModel() {
        withObservationTracking {
            _ = model.content
            _ = model.isDirty
            _ = model.isSaving
            _ = model.isLoading
            _ = model.baseRevisionId
            _ = model.failure
            _ = model.conflict
            _ = canWrite()
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, !finished else { return }
                render()
                observeModel()
            }
        }
    }

    private func render() {
        let editable = model.content != nil && !isClosing && !finished
        subjectField.isEditable = editable
        textView.isEditable = editable
        modeControl.isEnabled = editable
        saveButton.isEnabled = editable && canWrite() && !model.isSaving && model.conflict == nil
        abandonButton.isEnabled = !isClosing && !finished
        if let content = model.content, !model.isDirty, !textView.hasMarkedText() {
            if !subjectField.stringValue.utf8.elementsEqual(content.subject.utf8) {
                subjectField.stringValue = content.subject
            }
            if !textView.string.utf8.elementsEqual(content.bodySource.utf8) {
                textView.string = content.bodySource
            }
            modeControl.selectedSegment = content.format == .plain ? 0 : 1
            textView.mode = content.format == .plain ? .plain : .markdown
        }
        window?.isDocumentEdited = model.isDirty
        window?.title = model.content.flatMap { $0.subject.isEmpty ? nil : $0.subject } ?? "New Message"
        if closeIntent == .abandonEdits {
            statusLabel.stringValue =
                model.isSaving ? "Waiting for an in-progress save…" : "Closing without another save…"
        } else if model.conflict != nil {
            statusLabel.stringValue =
                "Another window saved this draft. Your text remains here. Keep editing or explicitly close without saving. Keep-both recovery follows in a later step."
        } else if let failure = model.failure {
            statusLabel.stringValue =
                "Draft error: \(failure.localizedDescription) Try Save again, keep editing, or explicitly close without saving."
        } else if model.isLoading && model.content == nil {
            statusLabel.stringValue = "Loading draft…"
        } else if model.isSaving || isClosing {
            statusLabel.stringValue = "Saving…"
        } else if !canWrite() && (model.isDirty || model.baseRevisionId == nil) {
            statusLabel.stringValue =
                "Writes are unavailable. Keep this window open or explicitly close without saving."
        } else if model.isDirty || model.baseRevisionId == nil {
            statusLabel.stringValue = "Not saved"
        } else {
            statusLabel.stringValue = "Saved"
        }
    }
}

/// Queries remain usable, but every unbegun write rechecks current health.
/// The native service still performs its own authoritative admission check.
@MainActor
private final class ComposeSavePolicy: DraftStore {
    private let source: any DraftStore
    private let canWrite: @MainActor () -> Bool
    init(source: any DraftStore, canWrite: @escaping @MainActor () -> Bool) {
        self.source = source
        self.canWrite = canWrite
    }
    func draft(id: String) async throws -> Draft { try await source.draft(id: id) }
    func saveDraft(
        idempotencyKey: String, draftId: String, baseRevisionId: String?,
        content: DraftContent
    ) async throws -> DraftSaved {
        try requireWrites()
        return try await source.saveDraft(
            idempotencyKey: idempotencyKey, draftId: draftId,
            baseRevisionId: baseRevisionId, content: content)
    }
    func discardDraft(idempotencyKey: String, draftId: String) async throws -> Receipt {
        try requireWrites()
        return try await source.discardDraft(idempotencyKey: idempotencyKey, draftId: draftId)
    }
    private func requireWrites() throws {
        guard canWrite() else {
            throw CoreFailure.unavailable(
                reason:
                    "Archive writes are unavailable. Keep this draft open or explicitly close without saving."
            )
        }
    }
}
