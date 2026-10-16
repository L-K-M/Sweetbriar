// The Composer Probe window (plan M0.11, spec/08 "Debug menu"): the
// composer's editor on its own — the Plain/Markdown switch persisted
// in user defaults, the editor in a scroll view, and the status line
// that reports whether the view stayed on TextKit 2 — so the owner's
// checklist (CJK marked text, right-to-left, undo) runs on the CI
// artifact. No core: the probe is the editor alone.

import AppKit

/// Owns the probe window: the mode switch, the scroll-hosted
/// `ComposerTextView`, and the TextKit status line.
@MainActor
final class ComposerProbeWindowController: NSWindowController {
    /// The window's starting content size — it stays resizable so the
    /// owner can stretch the editor.
    static let contentSize = NSSize(width: 640, height: 480)

    /// The editor under test — internal so the app-hosted tests can
    /// drive it.
    let textView = ComposerTextView()

    /// Plain/Markdown — persisted under the mode's one defaults key.
    private let modeControl: NSSegmentedControl

    /// The status line: "TextKit 2", or "TextKit 1 fallback!" if a
    /// TextKit 1 API slipped in — the regression the probe exists to
    /// catch.
    private let statusLabel = NSTextField(labelWithString: "")

    /// Keeps the status line honest when the text changes.
    private var textChangeObserver: (any NSObjectProtocol)?

    /// Called when the window closes — the app drops its reference so
    /// the next open builds a fresh controller.
    var onWindowClosed: (@MainActor () -> Void)?

    init() {
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.contentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Composer Probe"
        window.contentMinSize = NSSize(width: 400, height: 240)
        let content = NSView(frame: NSRect(origin: .zero, size: Self.contentSize))
        window.contentView = content
        modeControl = NSSegmentedControl(
            labels: Self.segmentTitles,
            trackingMode: .selectOne,
            target: nil,
            action: nil
        )
        super.init(window: window)
        window.delegate = self

        // The target lands only after super.init — a menu-less
        // control with a dead target does nothing.
        modeControl.target = self
        modeControl.action = #selector(modeChanged(_:))
        modeControl.selectedSegment = Self.segmentIndex(for: .stored)
        textView.mode = .stored

        // The editor fills the scroll view's width and grows down —
        // the standard scrollable-text configuration, TextKit 2
        // included.
        let scrollView = NSScrollView()
        scrollView.hasVerticalScroller = true
        scrollView.autohidesScrollers = true
        scrollView.borderType = .noBorder
        scrollView.documentView = textView
        textView.minSize = NSSize(width: 0, height: scrollView.contentSize.height)
        textView.maxSize = NSSize(
            width: CGFloat.greatestFiniteMagnitude,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.isVerticallyResizable = true
        textView.isHorizontallyResizable = false
        textView.autoresizingMask = [.width]
        textView.textContainer?.containerSize = NSSize(
            width: scrollView.contentSize.width,
            height: CGFloat.greatestFiniteMagnitude
        )
        textView.textContainer?.widthTracksTextView = true

        statusLabel.textColor = .secondaryLabelColor
        statusLabel.font = .systemFont(ofSize: NSFont.smallSystemFontSize)

        modeControl.translatesAutoresizingMaskIntoConstraints = false
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(modeControl)
        content.addSubview(scrollView)
        content.addSubview(statusLabel)
        NSLayoutConstraint.activate([
            modeControl.leadingAnchor.constraint(
                equalTo: content.leadingAnchor, constant: 12
            ),
            modeControl.topAnchor.constraint(
                equalTo: content.topAnchor, constant: 12
            ),
            scrollView.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            scrollView.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            scrollView.topAnchor.constraint(
                equalTo: modeControl.bottomAnchor, constant: 12
            ),
            scrollView.bottomAnchor.constraint(
                equalTo: statusLabel.topAnchor, constant: -8
            ),
            statusLabel.leadingAnchor.constraint(
                equalTo: content.leadingAnchor, constant: 12
            ),
            statusLabel.bottomAnchor.constraint(
                equalTo: content.bottomAnchor, constant: -8
            ),
        ])

        updateStatus()
        textChangeObserver = NotificationCenter.default.addObserver(
            forName: NSText.didChangeNotification,
            object: textView,
            queue: .main
        ) { [weak self] _ in
            // Posted on the main queue; the block itself isn't
            // actor-isolated.
            MainActor.assumeIsolated {
                self?.updateStatus()
            }
        }
    }

    required init?(coder: NSCoder) {
        fatalError("ComposerProbeWindowController is built in code")
    }

    /// The segment order — Plain first, as the spec lists them.
    private static let segmentTitles = ["Plain", "Markdown"]

    /// Which segment a mode maps to.
    private static func segmentIndex(for mode: ComposerMode) -> Int {
        switch mode {
        case .plain:
            return 0
        case .markdown:
            return 1
        }
    }

    /// Which mode a segment index maps to — anything but Plain's is
    /// Markdown, so an out-of-range index can't invent one.
    private static func mode(for segment: Int) -> ComposerMode {
        segment == segmentIndex(for: .plain) ? .plain : .markdown
    }

    /// The Plain/Markdown switch: restyle the editor and persist the
    /// choice (spec/08).
    @objc
    private func modeChanged(_ sender: NSSegmentedControl) {
        let mode = Self.mode(for: sender.selectedSegment)
        textView.mode = mode
        ComposerMode.stored = mode
        updateStatus()
    }

    /// The status line's current truth: TextKit 2, or the fallback
    /// the probe exists to catch.
    private func updateStatus() {
        statusLabel.stringValue =
            textView.isOnTextKit2 ? "TextKit 2" : "TextKit 1 fallback!"
    }
}

extension ComposerProbeWindowController: NSWindowDelegate {
    /// Closing drops the observer and hands the app's reference back
    /// — the next open builds a fresh window.
    func windowWillClose(_ notification: Notification) {
        if let textChangeObserver {
            NotificationCenter.default.removeObserver(textChangeObserver)
            self.textChangeObserver = nil
        }
        onWindowClosed?()
    }
}
