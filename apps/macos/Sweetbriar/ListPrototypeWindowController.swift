// The List Prototype window (spec/08 "Debug menu"): a fixed-size
// window listing `DebugSynthetic` through a `ListWindowModel`, driving
// either implementation. It owns the model's `DisplayLinkClock`, shows
// the fill hint while the source is empty, and closes the model when
// the window closes.

import AppKit
import SweetbriarBridge

/// Owns the prototype window: its `ListWindowModel`, the active list
/// (an `NSView` conforming to `PrototypeList`), and the status line —
/// the empty-source hint or an open failure. Switching the
/// implementation closes the old model and installs a fresh list, so
/// each implementation starts clean (spec/08).
@MainActor
final class ListPrototypeWindowController: NSWindowController {
    /// The fixed content size both lists measure against — the
    /// benchmark's runs are comparable because it never varies.
    static let contentSize = NSSize(width: 900, height: 640)

    private let session: CoreSession
    private var implementation: PrototypeImplementation
    private let formatter = MessageRowFormatter()

    /// The resolved core — cached so an implementation switch reuses
    /// it; `bridgeTask` dedupes concurrent resolves.
    private var bridge: CoreBridge?
    private var bridgeTask: Task<CoreBridge, any Error>?

    private var clock: DisplayLinkClock?
    private var model: ListWindowModel?
    private var list: (any PrototypeList)?
    private var openError: (any Error)?

    /// True from an install's start until its `open()` resolves — the
    /// status line reads "Opening" instead of the fill hint.
    private var opening = false

    /// Bumped per `install`: two installs can interleave at their
    /// suspension points (the Debug menu can race a manual swap), and
    /// a superseded one must not commit its list. The final stretch —
    /// building and embedding the list — runs without suspending.
    private var installSerial = 0

    /// The status overlay: the empty-source hint, or why nothing
    /// showed. Centered over the empty list.
    private let statusLabel = NSTextField(labelWithString: "")

    /// Called when the window closes — the app drops its reference so
    /// the next open builds a fresh controller.
    var onWindowClosed: (@MainActor () -> Void)?

    init(session: CoreSession, implementation: PrototypeImplementation) {
        self.session = session
        self.implementation = implementation
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.contentSize),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        // A fixed content size — and plain-titled, never
        // full-size content.
        window.setContentSize(Self.contentSize)
        window.contentMinSize = Self.contentSize
        window.contentMaxSize = Self.contentSize
        let content = NSView(frame: NSRect(origin: .zero, size: Self.contentSize))
        window.contentView = content
        super.init(window: window)
        window.delegate = self

        // The batcher's frame source, parked on this window's content
        // (spec/08: a `DisplayLinkClock` on the content view).
        clock = DisplayLinkClock(view: content)

        statusLabel.alignment = .center
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.isHidden = true
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(statusLabel)
        NSLayoutConstraint.activate([
            statusLabel.centerXAnchor.constraint(
                equalTo: content.centerXAnchor
            ),
            statusLabel.centerYAnchor.constraint(
                equalTo: content.centerYAnchor
            ),
            statusLabel.widthAnchor.constraint(
                lessThanOrEqualTo: content.widthAnchor, constant: -40
            ),
        ])
        updateTitle()
    }

    required init?(coder: NSCoder) {
        fatalError("ListPrototypeWindowController is built in code")
    }

    /// The window's frame-bearing name — "List Prototype — AppKit" or
    /// "— SwiftUI".
    private func updateTitle() {
        window?.title = "List Prototype — \(implementation.displayName)"
    }

    /// Resolve the core, then install the current implementation's
    /// model and list. A failed open lands in the window (spec/08).
    func openList() {
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                try await self.install(implementation: self.implementation)
            } catch {
                self.openError = error
                self.updateStatus()
            }
        }
    }

    /// Debug → Fill's follow-up: rebuild the open model so it lists
    /// the new generation (spec/08 — the prototype has no
    /// subscriptions, so the menu hands the generation over).
    func modelDidRefill() async throws {
        try await model?.rebuild()
    }

    /// The current list under test, for part B2b's harness.
    var prototypeList: (any PrototypeList)? {
        list
    }

    /// The current model under test, for part B2b's harness — its
    /// generation check and page-delivery hook.
    var listModel: ListWindowModel? {
        model
    }

    /// The last open or install failure, for the harness's checks.
    var openFailure: (any Error)? {
        openError
    }

    /// The benchmark's end of run: close the window, then await the
    /// model's `close()` so the run's reads are done before the next
    /// window opens. `windowWillClose` also closes the model — a
    /// second call is a no-op.
    func closeAndAwaitModel() async {
        window?.close()
        await model?.close()
    }

    /// Swap implementations: close the old model, install a fresh
    /// model and list — each starts clean (spec/08).
    func setImplementation(_ newValue: PrototypeImplementation) async {
        guard newValue != implementation else { return }
        implementation = newValue
        updateTitle()
        do {
            try await install(implementation: newValue)
        } catch {
            openError = error
            updateStatus()
        }
    }

    /// Tear down the current list, then install `implementation`'s:
    /// a fresh model over `DebugSynthetic` (whose pages land through
    /// the window's clock) inside the matching list view.
    private func install(
        implementation: PrototypeImplementation
    ) async throws {
        installSerial += 1
        let serial = installSerial
        opening = true
        updateStatus()
        await teardown()
        let bridge = try await resolveBridge()
        guard let content = window?.contentView, let clock else { return }
        // A later install claimed the window while this one suspended.
        guard serial == installSerial else { return }

        let model = ListWindowModel(
            source: bridge, spec: .debugSynthetic, clock: clock
        )
        let list: any PrototypeList
        switch implementation {
        case .appkit:
            list = AppKitPrototypeList(model: model, formatter: formatter) {
                [weak self] in
                self?.updateStatus()
            }
        case .swiftui:
            list = SwiftUIPrototypeList(model: model, formatter: formatter) {
                [weak self] in
                self?.updateStatus()
            }
        }
        let view: NSView = list
        view.frame = content.bounds
        view.autoresizingMask = [.width, .height]
        content.addSubview(view, positioned: .below, relativeTo: statusLabel)
        self.model = model
        self.list = list
        updateStatus()

        Task { @MainActor [weak self] in
            guard let self, self.model === model else { return }
            defer {
                self.opening = false
                self.updateStatus()
            }
            do {
                try await model.open()
            } catch {
                self.openError = error
            }
        }
    }

    /// Remove the current list and close its model — `close()` is
    /// uncancellable, so the teardown awaits it before installing
    /// fresh state.
    private func teardown() async {
        let old = model
        model = nil
        openError = nil
        list?.removeFromSuperview()
        list = nil
        await old?.close()
    }

    /// The open core, resolved once; concurrent callers share the one
    /// task.
    private func resolveBridge() async throws -> CoreBridge {
        if let bridge { return bridge }
        let task = bridgeTask ?? Task { try await session.bridge() }
        bridgeTask = task
        let resolved = try await task.value
        bridge = resolved
        return resolved
    }

    /// What the status line shows: an open failure, the fill hint
    /// while the source is empty (generation 0), or nothing.
    private func updateStatus() {
        if let openError {
            showStatus(
                "Couldn't open the list: \(openError.localizedDescription)",
                kind: .failure
            )
        } else if model == nil || opening {
            showStatus("Opening the core…")
        } else if model?.generation == 0 {
            showStatus("Debug → Fill 500,000 Synthetic Rows")
        } else {
            statusLabel.isHidden = true
        }
    }

    /// Why the status line is showing — the fill hint reads quietly,
    /// a failure reads red.
    private enum StatusKind {
        case hint
        case failure
    }

    private func showStatus(_ text: String, kind: StatusKind = .hint) {
        statusLabel.stringValue = text
        statusLabel.textColor =
            kind == .failure ? .systemRed : .secondaryLabelColor
        statusLabel.isHidden = false
    }
}

extension ListPrototypeWindowController: NSWindowDelegate {
    /// spec/07: the shell closes every window it opens — the model's
    /// handle included. `close()` is uncancellable; run it past the
    /// window's death, and drop the clock's display link.
    func windowWillClose(_ notification: Notification) {
        let model = self.model
        Task { @MainActor in
            await model?.close()
        }
        clock?.invalidate()
        onWindowClosed?()
    }
}
