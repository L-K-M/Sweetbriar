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
    private var loading: Task<Void, Never>?
    private var closed = false

    /// `target` is the row as the list showed it, so a Proton account's
    /// unread message is marked read; nil marks nothing.
    init(
        occurrenceId: Int64, reader: ReaderViewController, markRead: MarkReadTimer?,
        target: FlagTarget?
    ) {
        self.occurrenceId = occurrenceId
        self.reader = reader
        self.markRead = markRead
        self.target = target
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
            markRead?.show(target)
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
        markRead?.cancel()
        reader.close()
        onClose?(self)
    }
}

/// The open message windows, one per occurrence.
@MainActor
final class MessageWindows {
    private(set) var controllers: [Int64: MessageWindowController] = [:]

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
        let controller = make()
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
