// The main window's message list (spec/08 "Message list"): the AppKit
// list over a `ListWindowModel` for the window the sidebar chose.
// Showing another window closes the old model first, and a later
// choice wins over one still opening. A status line covers the list
// while it opens, when it is empty, and when it failed.

import AppKit
import Observation
import SweetbriarBridge

@MainActor
final class MessageListViewController: NSViewController {
    private let bridge: CoreBridge
    private let formatter = MessageRowFormatter()
    private var clock: DisplayLinkClock?
    private(set) var model: ListWindowModel?
    private(set) var list: AppKitPrototypeList?
    private var openError: (any Error)?
    private var opening = false

    /// Which `show` owns the list: a later one turns earlier tickets
    /// stale.
    private var tickets = InstallTickets()

    private let statusLabel = NSTextField(labelWithString: "")

    /// The list's generation after each new index: the sidebar
    /// re-reads its counts when it is newer than what it shows.
    var onGeneration: (@MainActor (UInt64) -> Void)?

    /// The selection, by occurrence id, whenever it changes.
    var onSelectionChange: (@MainActor (Set<Int64>) -> Void)?

    /// Measurement hooks are installed before each model opens.
    var onPagesDeliveredForBenchmark: (@MainActor ([Int]) -> Void)?
    var onIndexChangedForBenchmark: (@MainActor (UInt64) -> Void)?
    private(set) var shownSpec: WindowSpec?
    private(set) var choiceSerial = 0
    var failureForBenchmark: (any Error)? { openError ?? model?.lastError }

    init(bridge: CoreBridge) {
        self.bridge = bridge
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) {
        fatalError("MessageListViewController is built in code")
    }

    override func loadView() {
        let content = NSView()
        statusLabel.alignment = .center
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.isHidden = true
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(statusLabel)
        NSLayoutConstraint.activate([
            statusLabel.centerXAnchor.constraint(equalTo: content.centerXAnchor),
            statusLabel.centerYAnchor.constraint(equalTo: content.centerYAnchor),
            statusLabel.widthAnchor.constraint(
                lessThanOrEqualTo: content.widthAnchor, constant: -40
            ),
        ])
        view = content
    }

    /// List `spec`: close the current model, then open a fresh one in
    /// a fresh list.
    func show(_ spec: WindowSpec) async {
        let ticket = tickets.next()
        shownSpec = spec
        choiceSerial += 1
        opening = true
        updateStatus()
        await teardown()
        guard tickets.isCurrent(ticket) else { return }

        let clock = self.clock ?? DisplayLinkClock(view: view)
        self.clock = clock
        let model = ListWindowModel(source: bridge, spec: spec, clock: clock)
        model.onPagesDeliveredForBenchmark = { [weak self] pages in
            self?.onPagesDeliveredForBenchmark?(pages)
        }
        let list = AppKitPrototypeList(
            model: model,
            formatter: formatter,
            accountMarker: .hidden
        ) { [weak self] in
            self?.indexDidChange()
        }
        list.onSelectionChange = { [weak self] ids in
            self?.onSelectionChange?(ids)
        }
        list.frame = view.bounds
        list.autoresizingMask = [.width, .height]
        view.addSubview(list, positioned: .below, relativeTo: statusLabel)
        self.model = model
        self.list = list
        onSelectionChange?([])

        observeFailures(of: model)
        do {
            try await model.open()
        } catch {
            guard tickets.isCurrent(ticket) else { return }
            openError = error
        }
        guard tickets.isCurrent(ticket) else { return }
        opening = false
        updateStatus()
    }

    /// Close the list's model; the window is going away.
    func close() async {
        tickets.close()
        await teardown()
    }

    private func teardown() async {
        let old = model
        model = nil
        openError = nil
        list?.removeFromSuperview()
        list = nil
        await old?.close()
    }

    /// Show the model's read failures as they happen: a page read or
    /// the subscription can fail after the list opened.
    private func observeFailures(of model: ListWindowModel) {
        withObservationTracking {
            _ = model.lastError
        } onChange: { [weak self, weak model] in
            Task { @MainActor [weak self, weak model] in
                guard let self, let model, self.model === model else { return }
                self.updateStatus()
                self.observeFailures(of: model)
            }
        }
    }

    private func indexDidChange() {
        updateStatus()
        if let model { onIndexChangedForBenchmark?(model.generation) }
        guard let model, model.hasExactIndex else { return }
        onGeneration?(model.generation)
    }

    /// What the status line shows: why the list failed, that it is
    /// opening, or that it is empty; hidden while rows show.
    private func updateStatus() {
        let text: String?
        if let openError {
            text = "Couldn't open this list: \(openError.localizedDescription)"
        } else if let lastError = model?.lastError {
            text = "Couldn't read this list: \(lastError.localizedDescription)"
        } else if opening, (model?.count ?? 0) == 0 {
            text = "Opening…"
        } else if let model, model.hasExactIndex, model.count == 0 {
            text = "No messages"
        } else {
            text = nil
        }
        statusLabel.stringValue = text ?? ""
        statusLabel.isHidden = text == nil
    }
}

/// The reader's pane until the reader lands (spec/08, plan M1.5): it
/// says what is selected.
@MainActor
final class ReaderPlaceholderViewController: NSViewController {
    private let label = NSTextField(labelWithString: "")

    override func loadView() {
        let content = NSView()
        label.alignment = .center
        label.textColor = .secondaryLabelColor
        label.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(label)
        NSLayoutConstraint.activate([
            label.centerXAnchor.constraint(equalTo: content.centerXAnchor),
            label.centerYAnchor.constraint(equalTo: content.centerYAnchor),
            label.widthAnchor.constraint(lessThanOrEqualTo: content.widthAnchor, constant: -40),
        ])
        view = content
        show(selectionCount: 0)
    }

    /// Say how many messages are selected.
    func show(selectionCount count: Int) {
        switch count {
        case 0:
            label.stringValue = "No message selected"
        case 1:
            label.stringValue = "1 message selected. Reading arrives in a later update."
        default:
            label.stringValue = "\(count) messages selected"
        }
    }
}
