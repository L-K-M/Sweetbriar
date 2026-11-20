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
    let toRecipients: RecipientField
    let ccRecipients: RecipientField
    let bccRecipients: RecipientField
    private var recipientFields: [RecipientField] { [toRecipients, ccRecipients, bccRecipients] }
    let statusLabel = NSTextField(wrappingLabelWithString: "Loading draft…")
    let cueStatusLabel = NSTextField(wrappingLabelWithString: "")
    let identityStrip = IdentityStripAccessory(nibName: nil, bundle: nil)
    private let appearance: AppearanceModel?
    private let cue: ComposeCueModel?
    private var announceRoute: @MainActor (String) -> Void
    private let modeControl = NSSegmentedControl(
        labels: ["Plain", "Markdown"], trackingMode: .selectOne,
        target: nil, action: nil)
    private let saveButton = NSButton(title: "Save", target: nil, action: nil)
    private let sendButton = NSButton(title: "Send", target: nil, action: nil)
    private let undoSendButton = NSButton(title: "Undo Send", target: nil, action: nil)
    private let sendStatusLabel = NSTextField(wrappingLabelWithString: "")
    private lazy var sendBar: NSStackView = {
        let bar = NSStackView(views: [sendStatusLabel, undoSendButton])
        bar.spacing = 8
        bar.isHidden = true
        return bar
    }()
    private let abandonButton = NSButton(title: "Close Without Saving…", target: nil, action: nil)
    let keepBothButton = NSButton(title: "Keep Both", target: nil, action: nil)
    /// Opens a Keep Both copy (the window owner's `openCopy`).
    typealias OpenCopy = @MainActor (DraftSource, DraftContent, String, String) async throws -> Void
    private let openCopy: OpenCopy?
    /// A Keep Both whose create or save began: its exact keys and content
    /// are kept for an explicit retry, and the editor stays frozen.
    private struct KeepBothIntent {
        let source: DraftSource
        let content: DraftContent
        let createKey: String
        let saveKey: String
    }
    private var keepBothIntent: KeepBothIntent?
    private(set) var keepingBoth: Task<Void, Never>?
    private(set) var keepBothFailure: (any Error)?
    private lazy var autosave = DraftAutosave(
        model: model,
        prepareForSave: { [weak self] in
            await self?.prepareRecipients() ?? .refused
        })
    private let canWrite: @MainActor () -> Bool
    private let catalogSource: any ComposeCatalogSource
    private let preferredAccountId: String?
    private let sendSource: (any SendSource)?
    private let sendSettingsSource: (any SendSafetySettingsSource)?
    /// Built once, from the sources this archive actually offers; nil
    /// keeps Send disabled.
    private(set) lazy var sendModel: ComposeSendModel? = {
        guard let sendSource, let sendSettingsSource else { return nil }
        return ComposeSendModel(
            source: sendSource, settingsSource: sendSettingsSource, draftId: model.draftId,
            settlePendingSave: { [weak self] in
                guard let self else { return nil }
                return try await self.settlePendingSaveForSend()
            },
            closeAfterHold: { [weak self] in
                Task { @MainActor [weak self] in _ = await self?.closeAndWait() }
            },
            confirmCrossAccount: { [weak self] notice in
                await self?.confirmCrossAccountSend(notice) ?? false
            },
            isCrossAccountAcknowledged: { [weak self] ack in
                self?.crossAccount.isAcknowledged(ack) == true
            })
    }()
    private(set) lazy var identityControls: ComposeIdentityControls = {
        guard let window else { preconditionFailure("the compose window is installed before its controls") }
        return ComposeIdentityControls(
            draft: model, source: catalogSource, preferredAccountId: preferredAccountId,
            textView: textView, window: window,
            editable: { [weak self] in self?.canChangeIdentity == true },
            inputReady: { [weak self] in self?.identityInputReady == true },
            prepareInput: { [weak self] in await self?.prepareIdentityInput() ?? false },
            changed: { [weak self] in self?.identityChanged($0) })
    }()
    let publications: DraftAttachmentPublications
    /// This window's cross-account banner state (spec/08 "Cross-account
    /// check"): the current revision's notice and its memory-only
    /// acknowledgements, shared with Send's confirmation.
    let crossAccount = CrossAccountBannerModel()
    private let crossAccountLabel = NSTextField(wrappingLabelWithString: "")
    private let crossAccountUseButton = NSButton(title: "", target: nil, action: nil)
    private let crossAccountKeepButton = NSButton(title: "", target: nil, action: nil)
    private lazy var crossAccountBannerView: NSStackView = {
        crossAccountLabel.textColor = .labelColor
        crossAccountLabel.setAccessibilityIdentifier("crossAccountBannerLabel")
        crossAccountUseButton.target = self
        crossAccountUseButton.action = #selector(useCrossAccountSource)
        crossAccountUseButton.setAccessibilityIdentifier("crossAccountUse")
        crossAccountKeepButton.target = self
        crossAccountKeepButton.action = #selector(keepCrossAccountRoute)
        crossAccountKeepButton.setAccessibilityIdentifier("crossAccountKeep")
        let stack = NSStackView(
            views: [crossAccountLabel, crossAccountUseButton, crossAccountKeepButton])
        stack.orientation = .horizontal
        stack.spacing = 8
        stack.alignment = .centerY
        stack.setAccessibilityIdentifier("crossAccountBanner")
        crossAccountLabel.setContentHuggingPriority(.defaultLow, for: .horizontal)
        stack.isHidden = true
        return stack
    }()
    private(set) lazy var attachmentControls: ComposeAttachmentControls = {
        guard let window else { preconditionFailure("the compose window is installed before its controls") }
        return ComposeAttachmentControls(
            model: model, publications: publications, window: window,
            admitted: { [weak self] in self?.canAttach == true },
            changed: { [weak self] in self?.attachmentsChanged() },
            breakCoalescing: { [weak self] in self?.textView.breakUndoCoalescing() })
    }()
    private var loadTask: Task<Void, Never>?
    private var closing: Task<Bool, Never>?
    private var closeIntent: CloseMode?
    private var abandonAlert: NSAlert?
    /// Send's cross-account confirmation, while its sheet is up: the
    /// window's close ends it and cancels the send. The flag fences the
    /// handler and the close against resuming the continuation twice.
    private var crossAccountConfirmation: (
        alert: NSAlert, continuation: CheckedContinuation<Bool, Never>)?
    private var crossAccountConfirmationResumed = false
    var isClosing: Bool { closeIntent != nil }
    private var finished = false
    var onClosed: (@MainActor () -> Void)?

    init(
        source: any DraftStore & RecipientSource & ComposeCatalogSource, draftId: String,
        preferredAccountId: String? = nil, appearance: AppearanceModel? = nil, cue: ComposeCueModel? = nil,
        canWrite: @escaping @MainActor () -> Bool = { true },
        openCopy: OpenCopy? = nil,
        sendSource: (any SendSource)? = nil,
        settingsSource: (any SendSafetySettingsSource)? = nil,
        announceRoute: @escaping @MainActor (String) -> Void = { message in
            NSAccessibility.post(
                element: NSApp!, notification: .announcementRequested,
                userInfo: [.announcement: message, .priority: NSAccessibilityPriorityLevel.medium.rawValue])
        }
    ) {
        self.appearance = appearance
        self.cue = cue
        self.announceRoute = announceRoute
        self.canWrite = canWrite
        self.openCopy = openCopy
        self.sendSource = sendSource
        self.sendSettingsSource = settingsSource
        catalogSource = source
        self.preferredAccountId = preferredAccountId
        let policy = ComposeSavePolicy(source: source, canWrite: canWrite)
        model = DraftModel(source: policy, draftId: draftId)
        publications = DraftAttachmentPublications(model: model, source: policy)
        toRecipients = RecipientField(label: "To", source: source)
        ccRecipients = RecipientField(label: "Cc", source: source)
        bccRecipients = RecipientField(label: "Bcc", source: source)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 680, height: 630),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered, defer: false)
        window.title = "New Message"
        window.isReleasedWhenClosed = false
        window.contentMinSize = NSSize(width: 600, height: 430)
        window.setAccessibilityIdentifier("compose-\(draftId)")
        window.identifier = ComposeRestoration.windowIdentifier
        window.restorationClass = ComposeRestoration.self
        window.isRestorable = true
        super.init(window: window)
        window.delegate = self
        window.addTitlebarAccessoryViewController(identityStrip)
        identityStrip.strip.appearanceDidChange = { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, !finished else { return }
                renderChrome()
            }
        }
        buildContent(window)
        installRouteAnnouncementRecorder(in: window)
        render()
        observeModel()
        observeBanner()
        observeSend()
        observeAppearance()
        observeCue()
        loadTask = Task { [self] in
            await model.load()
            render()
            if !isClosing, !finished { identityControls.startDiscovery() }
        }
    }

    required init?(coder: NSCoder) { fatalError("ComposeWindowController is built in code") }

    /// `-recordRouteAnnouncements YES` (UI-test hook, spec/11 "UI"): the
    /// default `announceRoute` additionally writes the last message into a
    /// `RouteAnnouncementRecorder` in this window, so the From-switch test
    /// can read it. Without the argument nothing changes: no element and
    /// the same announcement.
    private static let recordRouteAnnouncementsKey = "recordRouteAnnouncements"

    private func installRouteAnnouncementRecorder(in window: NSWindow) {
        guard UserDefaults.standard.bool(forKey: Self.recordRouteAnnouncementsKey) else { return }
        let recorder = RouteAnnouncementRecorder(frame: NSRect(x: 0, y: 0, width: 1, height: 1))
        window.contentView?.addSubview(recorder)
        let post = announceRoute
        announceRoute = { message in
            post(message)
            recorder.record(message)
        }
    }

    private func buildContent(_ window: NSWindow) {
        let recipientRows = zip(["To:", "Cc:", "Bcc:"], recipientFields).map { title, field in
            field.inputDidChange = { [weak self] in self?.recipientInputDidChange() }
            field.editingDidEnd = { [weak self] in self?.saveNow() }
            field.presentationDidChange = { [weak self] in self?.render() }
            let label = NSTextField(labelWithString: title)
            label.alignment = .right
            label.widthAnchor.constraint(equalToConstant: 32).isActive = true
            let row = NSStackView(views: [label, field.control])
            row.spacing = 8
            field.control.setContentHuggingPriority(.defaultLow, for: .horizontal)
            return row
        }
        subjectField.placeholderString = "Subject"
        subjectField.setAccessibilityLabel("Subject")
        subjectField.setAccessibilityIdentifier("composeSubject")
        subjectField.delegate = self
        textView.delegate = self
        textView.setAccessibilityIdentifier("composeBody")
        textView.fileDropHandler = { [weak self] urls in self?.attachmentControls.attach(urls) ?? false }
        modeControl.target = self
        modeControl.action = #selector(modeChanged)
        saveButton.target = self
        saveButton.action = #selector(saveNow)
        saveButton.setAccessibilityIdentifier("composeSave")
        abandonButton.target = self
        abandonButton.action = #selector(confirmAbandon)
        keepBothButton.target = self
        keepBothButton.action = #selector(keepBoth)
        keepBothButton.setAccessibilityIdentifier("composeKeepBoth")
        keepBothButton.isHidden = true
        abandonButton.setAccessibilityIdentifier("composeCloseWithoutSaving")
        statusLabel.setAccessibilityIdentifier("composeSaveStatus")
        statusLabel.textColor = .secondaryLabelColor
        cueStatusLabel.setAccessibilityIdentifier("composeAppearanceStatus")
        cueStatusLabel.textColor = .secondaryLabelColor
        let identity = NSStackView(views: [
            NSTextField(labelWithString: "From:"), identityControls.popup, identityControls.refreshButton,
        ])
        identity.spacing = 8
        identityControls.popup.setContentHuggingPriority(.defaultLow, for: .horizontal)
        sendButton.target = self
        sendButton.action = #selector(send)
        sendButton.setAccessibilityIdentifier("composeSend")
        undoSendButton.target = self
        undoSendButton.action = #selector(undoSend)
        undoSendButton.setAccessibilityIdentifier("composeUndoSend")
        sendStatusLabel.setAccessibilityIdentifier("composeSendStatus")
        sendStatusLabel.textColor = .secondaryLabelColor
        let controls = NSStackView(views: [
            modeControl, attachmentControls.attachButton, saveButton, sendButton, keepBothButton,
            abandonButton,
        ])
        controls.spacing = 8
        let scroll = NSScrollView(frame: NSRect(x: 0, y: 0, width: 640, height: 340))
        scroll.hasVerticalScroller = true
        scroll.autohidesScrollers = true
        scroll.borderType = .bezelBorder
        scroll.drawsBackground = true
        scroll.backgroundColor = .textBackgroundColor
        textView.drawsBackground = true
        textView.backgroundColor = .textBackgroundColor
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
        let stack = NSStackView(
            views: [crossAccountBannerView, identity as NSView] + recipientRows + [
                subjectField, attachmentControls.list, controls, sendBar, scroll, statusLabel,
                cueStatusLabel,
            ])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 10
        stack.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(stack)
        window.contentView = content
        guard let contentGuide = window.contentLayoutGuide else {
            preconditionFailure("a titled compose window must have a content layout guide")
        }
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor, constant: 16),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor, constant: -16),
            NSLayoutConstraint(
                item: stack, attribute: .top, relatedBy: .equal,
                toItem: contentGuide, attribute: .top, multiplier: 1, constant: 16),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor, constant: -12),
            crossAccountBannerView.widthAnchor.constraint(equalTo: stack.widthAnchor),
            identity.widthAnchor.constraint(equalTo: stack.widthAnchor),
            subjectField.widthAnchor.constraint(equalTo: stack.widthAnchor),
            attachmentControls.list.widthAnchor.constraint(equalTo: stack.widthAnchor),
            sendBar.widthAnchor.constraint(equalTo: stack.widthAnchor),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor),
            scroll.heightAnchor.constraint(greaterThanOrEqualToConstant: 120),
            statusLabel.widthAnchor.constraint(equalTo: stack.widthAnchor),
            cueStatusLabel.widthAnchor.constraint(equalTo: stack.widthAnchor),
        ])
        for row in recipientRows { row.widthAnchor.constraint(equalTo: stack.widthAnchor).isActive = true }
        scroll.setContentHuggingPriority(.defaultLow, for: .vertical)
        window.center()
    }

    func controlTextDidChange(_ notification: Notification) { captureEdit() }
    func textDidChange(_ notification: Notification) { captureEdit() }
    @objc private func modeChanged() { captureEdit() }

    private func captureEdit() {
        guard !finished, !isClosing, !identityControls.isPublishing, var content = model.content else {
            return
        }
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
        if abandonAlert == nil, !identityControls.isDeciding { autosave.contentDidChange() }
        render()
    }

    private func recipientInputDidChange() {
        guard !finished, !isClosing, let content = model.content else { return }
        // Version raw input too: a prior receipt must not clear its dirty state
        // just because it cannot yet be represented as validated recipients.
        model.update(content)
        if abandonAlert == nil, !identityControls.isDeciding { autosave.contentDidChange() }
        render()
    }

    private var canChangeIdentity: Bool {
        !finished && !isClosing && model.content != nil && keepingBoth == nil && keepBothIntent == nil
    }
    private var canAttach: Bool {
        canChangeIdentity && canWrite() && abandonAlert == nil && !identityControls.isDeciding
    }

    private func attachmentsChanged() {
        guard !finished else { return }
        if !isClosing, abandonAlert == nil, !identityControls.isDeciding { autosave.contentDidChange() }
        render()
    }
    private var identityInputReady: Bool {
        canChangeIdentity && !recipientFields.contains(where: \.isPending)
            && !textView.hasMarkedText()
            && (window?.firstResponder as? NSTextView)?.hasMarkedText() != true
    }

    private func prepareIdentityInput() async -> Bool {
        guard canChangeIdentity,
            (window?.firstResponder as? NSTextView)?.hasMarkedText() != true
        else { return false }
        captureEdit()
        guard await prepareRecipients() == .ready else { return false }
        captureEdit()
        return identityInputReady
    }

    private func identityChanged(_ event: ComposeIdentityControls.Event) {
        switch event {
        case .route:
            autosave.contentDidChange()
            render(routeChanged: true)
            announceRoute("From: \(identityStrip.strip.content.accessibilityLabel)")
            return
        case .decision:
            if identityControls.isDeciding {
                autosave.suspendDebounce()
            } else if !isClosing, !finished, abandonAlert == nil {
                autosave.contentDidChange()
            }
        case .presentation: break
        }
        render()
    }

    private func prepareRecipients() async -> DraftAutosave.Readiness {
        guard !identityControls.isDeciding else { return .refused }
        let versions = recipientFields.map(\.inputVersion)
        var prepared: [[DraftRecipient]] = []
        for field in recipientFields {
            guard let values = await field.prepare() else {
                render()
                return versions == recipientFields.map(\.inputVersion) ? .refused : .superseded
            }
            prepared.append(values)
        }
        guard versions == recipientFields.map(\.inputVersion),
            !recipientFields.contains(where: \.isPending)
        else { return .superseded }
        // Another native selection may have opened its sheet while this
        // preparation awaited parsing. Do not start a new write behind it.
        guard !identityControls.isDeciding else { return .refused }
        guard var content = model.content else { return .refused }
        if !RecipientField.sameBytes(content.to, prepared[0])
            || !RecipientField.sameBytes(content.cc, prepared[1])
            || !RecipientField.sameBytes(content.bcc, prepared[2])
        {
            content.to = prepared[0]
            content.cc = prepared[1]
            content.bcc = prepared[2]
            model.update(content)
        }
        render()
        return .ready
    }

    @objc func saveNow() {
        guard !isClosing, !finished, abandonAlert == nil, !identityControls.isDeciding else { return }
        captureEdit()
        Task { [self] in _ = await saveAndWait() }
    }

    func saveAndWait() async -> Bool {
        guard !isClosing, !finished, abandonAlert == nil, !identityControls.isDeciding else { return false }
        captureEdit()
        let saved = await autosave.flush()
        render()
        return saved
    }

    /// Settle the pending save exactly as Save/Close do, so the send
    /// queues the revision the user last saw saved (spec/05).
    private func settlePendingSaveForSend() async throws -> String? {
        guard !finished, !isClosing else {
            throw CoreFailure.unavailable(reason: "This draft is closing.")
        }
        captureEdit()
        // A file still publishing joins the revision being sent, as Close
        // settles it before its final save.
        await publications.settle()
        guard await autosave.flush() else {
            if let failure = model.failure { throw failure }
            if model.conflict != nil {
                throw CoreFailure.conflict(detail: "Another window saved this draft.")
            }
            throw CoreFailure.unavailable(reason: "Save this draft before sending it.")
        }
        render()
        return model.baseRevisionId
    }

    @objc private func send() {
        // The menu and keyboard reach this too: admit exactly what the
        // button's enabled state admits.
        guard canSendNow else { return }
        Task { [self] in _ = await sendModel?.send() }
    }

    @objc private func undoSend() {
        guard case .held = sendModel?.state else { return }
        Task { [self] in _ = await sendModel?.undo() }
    }

    func windowDidResignKey(_ notification: Notification) {
        guard !isClosing, !finished, abandonAlert == nil, !identityControls.isDeciding else { return }
        captureEdit()
        Task { [self] in
            _ = await autosave.flush()
            render()
        }
    }

    /// Keep Both (spec/08 "Saving"): after a conflict, save this window's
    /// content as a new draft in its own window and close this one; the
    /// draft the other window saved stays as it is. Never automatic.
    @objc func keepBoth() {
        guard model.conflict != nil, let openCopy, keepingBoth == nil, !isClosing, !finished,
            abandonAlert == nil, !identityControls.isDeciding, canWrite(), let window,
            !recipientFields.contains(where: \.hasMarkedText), window.makeFirstResponder(nil),
            !textView.hasMarkedText()
        else { return }
        captureEdit()
        autosave.suspendDebounce()
        keepBothFailure = nil
        keepingBoth = Task { [self] in
            defer {
                keepingBoth = nil
                render()
            }
            // A file still publishing joins the copy.
            await publications.settle()
            let intent: KeepBothIntent
            if let keepBothIntent {
                intent = keepBothIntent
            } else {
                do {
                    let copy = try await model.copyForKeepBoth()
                    intent = KeepBothIntent(
                        source: copy.source, content: copy.content,
                        createKey: UUID().uuidString, saveKey: UUID().uuidString)
                } catch {
                    // Nothing was written: editing resumes.
                    keepBothFailure = error
                    return
                }
            }
            keepBothIntent = intent
            do {
                try await openCopy(intent.source, intent.content, intent.createKey, intent.saveKey)
            } catch {
                keepBothFailure = error
                return
            }
            keepBothIntent = nil
            // The copy now holds this window's content: close without another save.
            _ = beginClose(mode: .abandonEdits)
        }
        render()
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
        cancelCrossAccountConfirmation()
        if mode == .save {
            identityControls.cancelDecision()
            autosave.suspendDebounce()
            if let alert = abandonAlert {
                window?.endSheet(alert.window, returnCode: .alertFirstButtonReturn)
                alert.window.orderOut(nil)
                abandonAlert = nil
            }
            // Let the field editor/input method commit before freezing controls.
            guard !recipientFields.contains(where: \.hasMarkedText),
                window?.makeFirstResponder(nil) != false, !textView.hasMarkedText()
            else {
                statusLabel.stringValue = "Finish composing text before closing this draft."
                return nil
            }
            captureEdit()
        } else {
            // Fence an unbegun deadline and publications before scheduling
            // the drain task; a begun publication's result is dropped.
            autosave.stop()
            attachmentControls.stop()
        }
        closeIntent = mode
        identityControls.cancelDecision()
        render()
        let task = Task { [self] in
            await loadTask?.value
            // A file that finishes publishing now joins the final save.
            if mode == .save { await publications.settle() }
            if mode == .save, model.content != nil, !(await autosave.flush()) {
                closeIntent = nil
                closing = nil
                identityControls.startDiscovery()
                render()
                return false
            }
            await attachmentControls.stopAndWait()
            await autosave.stopAndWait()
            for field in recipientFields { await field.stopAndWait() }
            await identityControls.stopAndWait()
            model.stop()
            finished = true
            window?.close()
            onClosed?()
            return true
        }
        closing = task
        return task
    }

    /// AppKit restoration keeps only the draft id (spec/08 "Windows").
    func window(_ window: NSWindow, willEncodeRestorableState state: NSCoder) {
        ComposeRestoration.encode(draftId: model.draftId, into: state)
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
            _ = publications.publications.count
            _ = canWrite()
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, !finished else { return }
                render()
                // Re-register before the notice query awaits, so a save
                // landing meanwhile still triggers its own refresh.
                observeModel()
                await refreshCrossAccountNotice()
            }
        }
    }

    private func observeBanner() {
        withObservationTracking {
            _ = crossAccount.isVisible
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, !finished else { return }
                render()
                observeBanner()
            }
        }
    }

    /// Re-query the cross-account notice when a load or a successful
    /// save moved the current revision (spec/08 "Cross-account check").
    /// A stale answer is dropped inside the banner model; a query
    /// failure hides the banner.
    private func refreshCrossAccountNotice() async {
        guard !finished, !isClosing, let sendSource,
            let revision = crossAccount.nextRevision(toQuery: model.baseRevisionId)
        else { return }
        await crossAccount.refresh(
            source: sendSource, draftId: model.draftId, revisionId: revision)
        guard !finished else { return }
        render()
    }

    /// The account's display name, as the identity catalog knows it;
    /// the id itself when no identity names it.
    private func accountDisplayName(_ accountId: String) -> String {
        identityControls.model.catalog?.identities.first { $0.accountId == accountId }?
            .accountName ?? accountId
    }

    /// The banner's text and buttons for the current notice (spec/08).
    private func renderCrossAccountBanner() {
        guard let notice = crossAccount.notice, crossAccount.isVisible else {
            crossAccountBannerView.isHidden = true
            return
        }
        crossAccountBannerView.isHidden = false
        crossAccountLabel.stringValue =
            "This reply goes out from \(accountDisplayName(notice.routeAccountId)), but "
            + "\(accountDisplayName(notice.sourceAccountId)) received the original."
        crossAccountUseButton.title = "Use \(accountDisplayName(notice.sourceAccountId))"
        crossAccountKeepButton.title = "Keep \(accountDisplayName(notice.routeAccountId))"
        // A send in flight owns the window, From and Keep included.
        crossAccountKeepButton.isEnabled = sendModel?.isBusy != true
        crossAccountUseButton.isEnabled =
            sendModel?.isBusy != true
            && CrossAccountBannerModel.useIdentity(
                for: notice, in: identityControls.model.catalog?.identities ?? []) != nil
    }

    /// Use the source account's default sendable identity (spec/08): the
    /// identity picker's normal route change saves a new revision, whose
    /// notice is then nil and the banner hides.
    @objc private func useCrossAccountSource() {
        guard sendModel?.isBusy != true, let notice = crossAccount.notice,
            let identity = CrossAccountBannerModel.useIdentity(
                for: notice, in: identityControls.model.catalog?.identities ?? [])
        else { return }
        identityControls.choose(identityId: identity.identityId)
    }

    /// Keep the route account: remember this notice's acknowledgement in
    /// memory for this window only (spec/05, 08).
    @objc private func keepCrossAccountRoute() {
        guard sendModel?.isBusy != true else { return }
        crossAccount.keepCurrentNotice()
        render()
    }

    /// Send's cross-account confirmation (spec/08): the window asks once
    /// more before the acknowledgement is passed.
    private func confirmCrossAccountSend(_ notice: CrossAccountNotice) async -> Bool {
        // One sheet at a time: a second ask while one is pending (the
        // send model's busy gate keeps this unreachable) is a Cancel, so
        // the pending continuation stays the one its sheet resumes.
        guard let window, !finished, crossAccountConfirmation == nil else { return false }
        return await withCheckedContinuation { continuation in
            let alert = NSAlert()
            alert.messageText = "Send from \(accountDisplayName(notice.routeAccountId))?"
            alert.informativeText =
                "The original arrived at \(accountDisplayName(notice.sourceAccountId))."
            alert.alertStyle = .warning
            alert.addButton(withTitle: "Send")
            alert.addButton(withTitle: "Cancel")
            crossAccountConfirmation = (alert, continuation)
            crossAccountConfirmationResumed = false
            alert.beginSheetModal(for: window) { [weak self] response in
                guard let self, !self.crossAccountConfirmationResumed else { return }
                self.crossAccountConfirmationResumed = true
                self.crossAccountConfirmation = nil
                continuation.resume(returning: response == .alertFirstButtonReturn)
            }
        }
    }

    /// End a confirmation sheet the window's close interrupted: the
    /// send is cancelled rather than left awaiting a dismissed alert.
    private func cancelCrossAccountConfirmation() {
        guard let confirmation = crossAccountConfirmation,
            !crossAccountConfirmationResumed
        else { return }
        crossAccountConfirmationResumed = true
        crossAccountConfirmation = nil
        window?.endSheet(confirmation.alert.window, returnCode: .alertSecondButtonReturn)
        confirmation.alert.window.orderOut(nil)
        confirmation.continuation.resume(returning: false)
    }

    private func observeSend() {
        withObservationTracking {
            _ = sendModel?.state
            _ = sendModel?.undoError
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, !finished else { return }
                render()
                observeSend()
            }
        }
    }

    private func observeAppearance() {
        withObservationTracking {
            _ = appearance?.appearance
            _ = appearance?.lastError
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, !finished else { return }
                render()
                observeAppearance()
            }
        }
    }

    private func observeCue() {
        withObservationTracking {
            _ = cue?.mode
            _ = cue?.lastError
            _ = cue?.isLoading
            _ = cue?.isStopped
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, !finished else { return }
                render()
                observeCue()
            }
        }
    }

    private var appearanceReadPending: Bool {
        guard let identity = identityControls.model.selectedIdentity, identity.sendable != .unavailable,
            let appearance
        else { return false }
        return appearance.appearance == nil && appearance.lastError == nil
    }

    private var identityAppearanceIsLoading: Bool {
        guard let content = model.content else { return model.failure == nil }
        guard content.identityId != nil else { return false }
        switch identityControls.model.readState {
        case .unloaded, .loading:
            return true
        case .loaded:
            return appearanceReadPending
        case .failed, .stopped:
            return false
        }
    }

    private func renderChrome() {
        guard let window else { return }
        var tint: NSColor?
        if cue?.mode == .strong, case .identity(_, _, let tokens) = identityStrip.strip.content {
            let dark =
                identityStrip.strip.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
            tint = appearanceColor(dark ? tokens.dark.stripSrgb : tokens.light.stripSrgb)
        }
        if let tint {
            if !window.styleMask.contains(.fullSizeContentView) {
                window.styleMask.insert(.fullSizeContentView)
            }
            if !window.titlebarAppearsTransparent { window.titlebarAppearsTransparent = true }
            if window.backgroundColor != tint { window.backgroundColor = tint }
        } else {
            if window.titlebarAppearsTransparent { window.titlebarAppearsTransparent = false }
            if window.styleMask.contains(.fullSizeContentView) {
                window.styleMask.remove(.fullSizeContentView)
            }
            if window.backgroundColor != .windowBackgroundColor {
                window.backgroundColor = .windowBackgroundColor
            }
        }
        let message: String?
        if let error = cue?.lastError {
            message =
                "Compose appearance may be out of date: \(error.localizedDescription). Quit and reopen Sweetbriar to retry."
        } else if cue == nil || cue?.isStopped == true {
            message = "Compose appearance is unavailable. Quit and reopen Sweetbriar to retry."
        } else if cue?.mode == nil {
            message = "Loading compose appearance…"
        } else if cue?.mode == .strong && tint == nil && !(model.content == nil && model.failure != nil) {
            message =
                identityAppearanceIsLoading
                ? "Loading compose appearance…"
                : "Strong appearance is unavailable for this sending identity."
        } else {
            message = nil
        }
        let text = message ?? ""
        if cueStatusLabel.stringValue != text { cueStatusLabel.stringValue = text }
        if cueStatusLabel.isHidden != (message == nil) { cueStatusLabel.isHidden = message == nil }
    }

    /// The resolved identity's effective account color as its palette
    /// name, or the `#rrggbb` itself when it is not a preset (spec/08
    /// "Identity strip and account colors"). Empty when the strip shows
    /// no resolved identity. Published through the strip's accessibility
    /// value so a UI test can observe a route's color.
    private func stripColorName() -> String {
        guard case .identity = identityStrip.strip.content,
            let identity = identityControls.model.selectedIdentity,
            let resolved = IdentityStripContent.matchingAppearance(
                for: identity, in: appearance?.appearance)
        else { return "" }
        let srgb = resolved.colorSrgb
        return appearance?.palette.first { $0.colorSrgb == srgb }?.name ?? srgb
    }

    /// The window's existing admission checks: a selected identity that
    /// can actually send, at least one stored recipient, an editing draft,
    /// no conflict or deciding sheet, no send already owning the window,
    /// and archive writes (spec/05).
    private var canSendNow: Bool {
        guard let content = model.content, let sendModel, !sendModel.isBusy, canWrite(), !finished,
            !isClosing,
            abandonAlert == nil, !identityControls.isDeciding, !model.isSaving, model.conflict == nil,
            model.draft?.state == .editing,
            let identity = identityControls.model.selectedIdentity,
            identity.sendable != .unavailable,
            // Only Proton sends in the pilot: SMTP is later (spec/05), so an
            // IMAP attempt would wait in the queue with nothing to send it.
            identity.accountKind == .proton
        else { return false }
        return !content.to.isEmpty || !content.cc.isEmpty || !content.bcc.isEmpty
    }

    private func renderSendChrome() {
        sendButton.isEnabled = canSendNow
        sendButton.toolTip =
            sendModel == nil
            ? "Sending needs an archive that can write and a sending identity."
            : "Send this draft from a Proton identity."
        guard let sendModel else {
            sendBar.isHidden = true
            return
        }
        switch sendModel.state {
        case .held(_, let remaining):
            sendBar.isHidden = false
            undoSendButton.isHidden = false
            undoSendButton.isEnabled = true
            sendStatusLabel.stringValue = sendModel.undoError.map {
                "Sending in \(remaining) s. Couldn't undo: \($0)"
            } ?? "Sending in \(remaining) s"
        case .failed(let message):
            sendBar.isHidden = false
            undoSendButton.isHidden = true
            sendStatusLabel.stringValue = message
        default:
            sendBar.isHidden = true
        }
    }

    private func render(routeChanged: Bool = false) {
        // A send in flight or a held attempt owns the window: the draft
        // can no longer be edited (spec/08).
        let busy = sendModel?.isBusy ?? false
        let editable = model.content != nil && !isClosing && !finished && keepingBoth == nil
            && keepBothIntent == nil && !busy
        subjectField.isEditable = editable
        textView.isEditable = editable
        modeControl.isEnabled = editable
        for field in recipientFields { field.control.isEditable = editable }
        saveButton.isEnabled = editable && canWrite() && !model.isSaving && model.conflict == nil
        attachmentControls.render()
        renderCrossAccountBanner()
        renderSendChrome()
        abandonButton.isEnabled = !isClosing && !finished && keepingBoth == nil
        keepBothButton.isHidden = model.conflict == nil || openCopy == nil
        keepBothButton.isEnabled = !isClosing && !finished && keepingBoth == nil && canWrite()
        if let content = model.content, !model.isDirty, !textView.hasMarkedText() {
            if !subjectField.stringValue.utf8.elementsEqual(content.subject.utf8) {
                subjectField.stringValue = content.subject
            }
            if !textView.string.utf8.elementsEqual(content.bodySource.utf8) {
                textView.string = content.bodySource
            }
            toRecipients.load(content.to)
            ccRecipients.load(content.cc)
            bccRecipients.load(content.bcc)
            modeControl.selectedSegment = content.format == .plain ? 0 : 1
            textView.mode = content.format == .plain ? .plain : .markdown
        }
        window?.isDocumentEdited = model.isDirty || recipientFields.contains(where: \.isPending)
        identityControls.appearanceSnapshot = appearance?.appearance
        identityControls.render()
        let stripContent: IdentityStripView.Content
        if let content = model.content {
            if content.identityId != nil && identityControls.isCatalogLoading
                && identityControls.model.selectedIdentity == nil
            {
                stripContent = .unavailable(label: "Loading sending identity…")
            } else if appearanceReadPending, let identity = identityControls.model.selectedIdentity {
                stripContent = .unavailable(
                    label: "Loading appearance: \(IdentityStripContent.label(for: identity))")
            } else {
                stripContent = IdentityStripContent.resolve(
                    route: content.identityId, identity: identityControls.model.selectedIdentity,
                    appearance: appearance?.appearance)
            }
        } else {
            stripContent = .unavailable(
                label: model.failure == nil ? "Loading sending identity…" : "Sending identity unavailable")
        }
        identityStrip.setContent(stripContent, routeChanged: routeChanged)
        identityStrip.strip.colorName = stripColorName()
        renderChrome()
        let subject = model.content.flatMap { $0.subject.isEmpty ? nil : $0.subject } ?? "New Message"
        window?.title = identityControls.titleContext.map { "\(subject) · \($0)" } ?? subject
        if closeIntent == .abandonEdits {
            statusLabel.stringValue =
                model.isSaving ? "Waiting for an in-progress save…" : "Closing without another save…"
        } else if keepingBoth != nil {
            statusLabel.stringValue = "Saving your text as a new draft…"
        } else if let failure = keepBothFailure {
            statusLabel.stringValue =
                keepBothIntent == nil
                ? "Keep Both: \(failure.localizedDescription) Your text remains here."
                : "Keep Both didn't finish: \(failure.localizedDescription) Your text remains here; try Keep Both again, or explicitly close without saving."
        } else if model.conflict != nil {
            statusLabel.stringValue =
                openCopy == nil
                ? "Another window saved this draft. Your text remains here. Keep editing or explicitly close without saving."
                : "Another window saved this draft. Your text remains here. Keep Both saves it as a new draft in its own window, or explicitly close without saving."
        } else if let failure = model.failure {
            statusLabel.stringValue =
                "Draft error: \(failure.localizedDescription) Try Save again, keep editing, or explicitly close without saving."
        } else if identityControls.isDeciding {
            statusLabel.stringValue = "Choose Keep Text or Append Signature. From has not changed."
        } else if let problem = identityControls.model.problem ?? identityControls.problem {
            statusLabel.stringValue = problem
        } else if case .failed(let error) = identityControls.model.readState {
            statusLabel.stringValue =
                "Identity catalog: \(error.localizedDescription) Try Refresh Identities."
        } else if let field = recipientFields.first(where: { $0.problem != nil }) {
            statusLabel.stringValue =
                "\(field.control.accessibilityLabel() ?? "Recipient"): \(field.problem ?? "Input is not ready.")"
        } else if recipientFields.contains(where: \.isPending) {
            statusLabel.stringValue = "Recipient input is not saved. Finish the address or try Save again."
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
    func publishDraftAttachment(file: URL) async throws -> DraftAttachmentObject {
        try requireWrites()
        return try await source.publishDraftAttachment(file: file)
    }
    func republishDraftAttachment(
        draftId: String, revisionId: String, reference: DraftAttachmentRef
    ) async throws -> DraftAttachmentObject {
        try requireWrites()
        return try await source.republishDraftAttachment(
            draftId: draftId, revisionId: revisionId, reference: reference)
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
