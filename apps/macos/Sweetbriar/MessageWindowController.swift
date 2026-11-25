// A message in its own window (spec/08 "Message window"): the main
// window's reader over one occurrence, one window per occurrence, and
// every one closed when the main window's content retires.

import AppKit
import SweetbriarBridge

/// One message's window: a reader over one occurrence that marks it
/// read as the main reader does, titled with its subject.
@MainActor
final class MessageWindowController: NSWindowController, NSWindowDelegate {
    static let defaultContentSize = NSSize(width: 720, height: 640)
    static let accessibilityID = "messageWindow"

    let occurrenceId: Int64
    let reader: ReaderViewController
    /// Told once, when the window closes for any reason.
    var onClose: (@MainActor (MessageWindowController) -> Void)?

    private let markRead: MarkReadTimer?
    private let target: FlagTarget?
    private let appearance: AppearanceModel?
    private let cue: ComposeCueModel?
    private var loading: Task<Void, Never>?
    private var closed = false
    private var displayOptionsObserver: NSObjectProtocol?
    /// The tint the window shows (spec/08 Message window "Tint"), if any.
    private(set) var tint: MessageWindowTint?
    /// Whether Increase Contrast is on; tests inject it.
    var increaseContrast: () -> Bool = { NSWorkspace.shared.accessibilityDisplayShouldIncreaseContrast } {
        didSet { renderTint() }
    }

    /// `target` is the row as the list showed it, so a Proton account's
    /// unread message is marked read; nil marks nothing. `appearance`
    /// and `cue` tint the window in Color Wash and Strong; without them
    /// it stays neutral.
    init(
        occurrenceId: Int64, reader: ReaderViewController, markRead: MarkReadTimer?,
        target: FlagTarget?, appearance: AppearanceModel? = nil, cue: ComposeCueModel? = nil
    ) {
        self.occurrenceId = occurrenceId
        self.reader = reader
        self.markRead = markRead
        self.target = target
        self.appearance = appearance
        self.cue = cue
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.defaultContentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = Self.title(nil)
        // Window restoration doesn't reopen message windows (spec/08).
        window.isRestorable = false
        window.isReleasedWhenClosed = false
        window.setAccessibilityIdentifier(Self.accessibilityID)
        window.contentViewController = reader
        window.setContentSize(Self.defaultContentSize)
        window.center()
        super.init(window: window)
        window.delegate = self
        reader.backdrop.appearanceDidChange = { [weak self] in self?.renderTint() }
        displayOptionsObserver = NSWorkspace.shared.notificationCenter.addObserver(
            forName: NSWorkspace.accessibilityDisplayOptionsDidChangeNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.renderTint() }
        }
        observeTint()
    }

    @available(*, unavailable)
    required init?(coder: NSCoder) { fatalError("init(coder:) has not been implemented") }

    /// Show the message, then title the window and start its read mark.
    func load() {
        loading = Task { [weak self] in
            guard let self else { return }
            await reader.show(selection: [occurrenceId])
            guard !closed, !Task.isCancelled else { return }
            window?.title = Self.title(reader.shownSubject)
            renderTint()
            markRead?.show(target)
        }
    }

    /// Tint the title bar and the reader's header area with the
    /// receiving account's strip fill in Color Wash and Strong; leave
    /// the window neutral otherwise (spec/08 Message window "Tint").
    func renderTint() {
        guard let window, !closed else { return }
        let accountId = reader.model.currentMessage?.accountId
        let tokens = accountId.flatMap { id in
            appearance?.appearance?.accounts.first { $0.accountId == id }?.tokens
        }
        let tint = MessageWindowTint.resolve(
            mode: cue?.mode, tokens: tokens, appearance: reader.view.effectiveAppearance,
            increaseContrast: increaseContrast())
        self.tint = tint
        if let tint {
            if !window.styleMask.contains(.fullSizeContentView) { window.styleMask.insert(.fullSizeContentView) }
            if !window.titlebarAppearsTransparent { window.titlebarAppearsTransparent = true }
            if window.backgroundColor != tint.fill { window.backgroundColor = tint.fill }
        } else {
            if window.titlebarAppearsTransparent { window.titlebarAppearsTransparent = false }
            if window.styleMask.contains(.fullSizeContentView) { window.styleMask.remove(.fullSizeContentView) }
            if window.backgroundColor != .windowBackgroundColor { window.backgroundColor = .windowBackgroundColor }
        }
        reader.applyHeaderTint(tint)
    }

    /// Follow the account's appearance and the compose cue mode.
    private func observeTint() {
        guard appearance != nil || cue != nil else { return }
        withObservationTracking {
            _ = appearance?.appearance
            _ = cue?.mode
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self, !self.closed else { return }
                self.renderTint()
                self.observeTint()
            }
        }
    }

    /// The subject, or "No Subject" for none or a blank one.
    static func title(_ subject: String?) -> String {
        guard let subject, !subject.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        else { return "No Subject" }
        return subject
    }

    /// Closing cancels a pending mark and drops what the reader still
    /// loads (spec/08 "Message window").
    func windowWillClose(_ notification: Notification) {
        guard !closed else { return }
        closed = true
        loading?.cancel()
        if let displayOptionsObserver {
            NSWorkspace.shared.notificationCenter.removeObserver(displayOptionsObserver)
            self.displayOptionsObserver = nil
        }
        markRead?.cancel()
        reader.close()
        onClose?(self)
    }
}

// MARK: - View commands (spec/08 "Reader")

extension MessageWindowController: ReaderCommands, NSMenuItemValidation {
    /// View → Message Source (⌥⌘U) for this window's message.
    @objc func toggleMessageSource(_ sender: Any?) {
        guard reader.canToggleSource else { return NSSound.beep() }
        reader.toggleSourceFromMenu()
    }

    /// View → Reading View (⌥⌘R) for this window's message.
    @objc func toggleReadingView(_ sender: Any?) {
        guard reader.canToggleReadingView else { return NSSound.beep() }
        reader.toggleReadingViewFromMenu()
    }

    func validateMenuItem(_ menuItem: NSMenuItem) -> Bool {
        switch menuItem.action {
        case #selector(toggleMessageSource(_:)):
            menuItem.state = reader.isShowingSource ? .on : .off
            return reader.canToggleSource
        case #selector(toggleReadingView(_:)):
            menuItem.state = reader.isShowingReadingView ? .on : .off
            return reader.canToggleReadingView
        default:
            return true
        }
    }
}

/// The open message windows, one per occurrence.
@MainActor
final class MessageWindows {
    private(set) var controllers: [Int64: MessageWindowController] = [:]
    /// Where the next new window's top left goes, so one doesn't hide
    /// the last; zero leaves the first one centered.
    private var cascade = NSPoint.zero

    /// Bring `occurrenceId`'s window to the front, or open one that
    /// `make` builds.
    @discardableResult
    func open(
        _ occurrenceId: Int64, make: () -> MessageWindowController
    ) -> MessageWindowController {
        if let open = controllers[occurrenceId] {
            open.showWindow(nil)
            return open
        }
        if controllers.isEmpty { cascade = .zero }
        let controller = make()
        if let window = controller.window { cascade = window.cascadeTopLeft(from: cascade) }
        controllers[occurrenceId] = controller
        controller.onClose = { [weak self] closed in
            guard let self, controllers[closed.occurrenceId] === closed else { return }
            controllers[closed.occurrenceId] = nil
        }
        controller.load()
        controller.showWindow(nil)
        return controller
    }

    /// The occurrence of the message window that is key, if one is.
    func occurrence(of window: NSWindow?) -> Int64? {
        guard let window else { return nil }
        return controllers.values.first { $0.window === window }?.occurrenceId
    }

    /// Close every window: the main window's content retires (spec/08
    /// "Message window", Old archive).
    func closeAll() {
        let open = Array(controllers.values)
        controllers = [:]
        for controller in open { controller.close() }
    }
}
