// The main window's message list (spec/08 "Message list"): the AppKit
// list over a `ListWindowModel` for the window the sidebar chose.
// Showing another window closes the old model first, and a later
// choice wins over one still opening. A status line covers the list
// while it opens, when it is empty, and when it failed; Search results
// carry a coverage strip below them with the index controls. While a
// rebuild's reset or enqueue keeps Search from opening, the strip shows
// polled standalone coverage, and Search reopens once it can (spec/08).
//
//   Search ──open/read: unavailable──► standalone: poll coverage
//     ▲                                     │
//     └──── coverage says Search can open ──┘  (once per generation)

import AppKit
import Observation
import SweetbriarBridge

@MainActor
final class MessageListViewController: NSViewController {
    private let bridge: CoreBridge
    private let searchModel: SearchFieldModel
    private let search: SearchFieldController
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

    /// Search coverage, collapsed while no Search status shows.
    private let coverage = SearchCoverageView()
    private lazy var coverageCollapsed = coverage.heightAnchor.constraint(equalToConstant: 0)

    /// The index controls, and standalone coverage while Search can't
    /// open (spec/08).
    private let indexControls: IndexControlModel
    /// Search couldn't open, or stopped reading, for an unavailable
    /// index: the strip shows standalone coverage until it says Search
    /// can open again.
    private var standalone = false
    /// When and at which coverage generation a reopen was last tried:
    /// once per change, or again after `reopenRetry`, so a reopen that
    /// fails again neither churns nor wedges on a quiet archive.
    private var lastReopen: (generation: UInt64, at: ContinuousClock.Instant)?
    private static let reopenRetry: Duration = .seconds(5)

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
    /// Search's field and model, for the keystroke benchmark (plan M1.6q2).
    var searchFieldForBenchmark: NSSearchField { search.fieldForBenchmark }
    var searchModelForBenchmark: SearchFieldModel { searchModel }

    init(bridge: CoreBridge, sidebar: SidebarModel) {
        self.bridge = bridge
        let model = SearchFieldModel(parser: bridge, label: { reference in
            switch reference {
            case .account(let id): return sidebar.sections.first { $0.id == id }?.title
            case .mailbox(let id):
                return sidebar.sections.flatMap(\.entries).first {
                    $0.target == .window(.mailbox(id))
                }?.title
            case .tag: return nil
            }
        })
        searchModel = model
        indexControls = IndexControlModel(source: bridge)
        search = SearchFieldController(model: model, scopes: {
            var values: [(label: String, scope: SearchScope)] = [("All accounts", .all)]
            for section in sidebar.sections where section.id != SidebarModel.unifiedSectionID {
                values.append((section.title, .account(section.id)))
                for entry in section.entries {
                    if case .window(.mailbox(let id)) = entry.target {
                        values.append(("\(section.title): \(entry.title)", .mailbox(id)))
                    }
                }
            }
            return values
        })
        super.init(nibName: nil, bundle: nil)
        coverage.onCommand = { [weak self] command in
            self?.perform(command)
        }
        observeControls()
        model.onPresent = { [weak self, weak model] spec, ticket in
            guard model?.isCurrent(ticket) == true else { return }
            self?.show(spec)
        }
    }

    required init?(coder: NSCoder) {
        fatalError("MessageListViewController is built in code")
    }

    override func loadView() {
        let content = NSView()
        addChild(search)
        search.view.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(search.view)
        statusLabel.alignment = .center
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.isHidden = true
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(statusLabel)
        coverage.isHidden = true
        coverage.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(coverage)
        NSLayoutConstraint.activate([
            coverage.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            coverage.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            coverage.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            coverageCollapsed,
            // The window's content runs under the title bar; the field
            // must sit below it to take clicks.
            search.view.topAnchor.constraint(equalTo: content.safeAreaLayoutGuide.topAnchor),
            search.view.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            search.view.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            statusLabel.centerXAnchor.constraint(equalTo: content.centerXAnchor),
            statusLabel.centerYAnchor.constraint(equalTo: content.centerYAnchor),
            statusLabel.widthAnchor.constraint(
                lessThanOrEqualTo: content.widthAnchor, constant: -40
            ),
        ])
        view = content
    }

    /// Poll standalone coverage only while the list shows.
    override func viewDidAppear() {
        super.viewDidAppear()
        if standalone, let spec = searchSpec {
            indexControls.startPolling(scope: spec.scope)
        }
    }

    /// Polling stops with the window; the durable job never does
    /// (spec/08).
    override func viewWillDisappear() {
        super.viewWillDisappear()
        indexControls.stopPolling()
    }

    /// List `spec`: close the current model, then open a fresh one in
    /// a fresh list.
    func show(_ spec: WindowSpec) {
        search.navigated(to: spec)
        standalone = false
        indexControls.stopPolling()
        let ticket = tickets.next()
        shownSpec = spec
        choiceSerial += 1
        opening = true
        updateStatus()
        updateCoverage()
        // Take the ticket at the user/field choice, before scheduling can reorder it.
        Task { @MainActor [weak self] in
            await self?.install(spec, ticket: ticket)
        }
    }

    private func install(_ spec: WindowSpec, ticket: Int) async {
        guard tickets.isCurrent(ticket) else { return }
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
            accountMarker: .hidden,
            // spec/08: reopen at Top once the selected message is no
            // longer a hit.
            selectionLoss: spec.isSearch ? .reopenAtTop : .keepPosition
        ) { [weak self] in
            self?.indexDidChange()
        }
        list.onSelectionChange = { [weak self] ids in
            self?.onSelectionChange?(ids)
        }
        list.translatesAutoresizingMaskIntoConstraints = false
        view.addSubview(list, positioned: .below, relativeTo: statusLabel)
        NSLayoutConstraint.activate([
            list.topAnchor.constraint(equalTo: search.view.bottomAnchor),
            list.leadingAnchor.constraint(equalTo: view.leadingAnchor),
            list.trailingAnchor.constraint(equalTo: view.trailingAnchor),
            list.bottomAnchor.constraint(equalTo: coverage.topAnchor),
        ])
        self.model = model
        self.list = list
        onSelectionChange?([])

        observe(model)
        do {
            try await model.open()
        } catch {
            guard tickets.isCurrent(ticket) else { return }
            openError = error
        }
        guard tickets.isCurrent(ticket) else { return }
        opening = false
        if Self.isUnavailable(openError) {
            enterStandalone()
        }
        updateStatus()
        updateCoverage()
    }

    /// Close the list's model; the window is going away.
    func close() async {
        indexControls.stopPolling()
        searchModel.close()
        tickets.close()
        await teardown()
    }

    private func teardown() async {
        let old = model
        model = nil
        openError = nil
        list?.removeFromSuperview()
        list = nil
        // The old results' note goes with them.
        updateCoverage()
        await old?.close()
    }

    /// Show the model's read failures and Search status as they
    /// happen: a page read or the subscription can fail after the list
    /// opened, and each landing snapshot brings its own status.
    private func observe(_ model: ListWindowModel) {
        withObservationTracking {
            _ = model.lastError
            _ = model.searchStatus
        } onChange: { [weak self, weak model] in
            Task { @MainActor [weak self, weak model] in
                guard let self, let model, self.model === model else { return }
                // A rebuild's reset makes an open Search's reads fail.
                if Self.isUnavailable(model.lastError) {
                    self.enterStandalone()
                }
                self.updateStatus()
                self.updateCoverage()
                self.observe(model)
            }
        }
    }

    /// Follow the controls' commands and standalone coverage.
    private func observeControls() {
        withObservationTracking {
            _ = indexControls.coverage
            _ = indexControls.pending
            _ = indexControls.commandError
            _ = indexControls.readError
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                guard let self else { return }
                self.controlsDidChange()
                self.observeControls()
            }
        }
    }

    /// Redraw, and reopen Search once standalone coverage says it can
    /// open — once per coverage change.
    private func controlsDidChange() {
        updateStatus()
        updateCoverage()
        guard standalone, let spec = shownSpec, let polled = indexControls.coverage,
              polled.searchCanOpen
        else { return }
        let now = ContinuousClock.now
        if let last = lastReopen, last.generation == polled.generation, now - last.at < Self.reopenRetry {
            return
        }
        lastReopen = (polled.generation, now)
        show(spec)
    }

    /// Search can't open or read: show standalone coverage, polled while
    /// the list shows, with the controls in view.
    private func enterStandalone() {
        guard let spec = searchSpec, !standalone else { return }
        standalone = true
        // Rows read before the reset are gone from the index.
        list?.isHidden = true
        coverage.disclose()
        if view.window != nil {
            indexControls.startPolling(scope: spec.scope)
        }
    }

    /// Run a control on the coverage it was offered on: standalone
    /// coverage while Search can't open, else the newer of the
    /// window's status and the last standalone read.
    private func perform(_ command: IndexControlModel.Command) {
        guard let spec = searchSpec else { return }
        let window = standalone ? nil : model?.searchStatus?.coverage
        guard let shown = indexControls.shown(with: window) else { return }
        Task {
            await indexControls.perform(command, on: shown, scope: spec.scope)
        }
    }

    private var searchSpec: SearchSpec? {
        guard case .search(let spec) = shownSpec else { return nil }
        return spec
    }

    private static func isUnavailable(_ error: (any Error)?) -> Bool {
        guard let failure = error as? CoreFailure, case .unavailable = failure else { return false }
        return true
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
        if standalone {
            text = Self.unavailableText(indexControls.coverage, failure: indexControls.readError)
        } else if let openError {
            text = "Couldn't open this list: \(openError.localizedDescription)"
        } else if let lastError = model?.lastError {
            text = "Couldn't read this list: \(lastError.localizedDescription)"
        } else if opening, (model?.count ?? 0) == 0 {
            text = "Opening…"
        } else if let model, model.hasExactIndex, model.count == 0 {
            text = shownSpec?.isSearch == true ? "No matching messages" : "No messages"
        } else {
            text = nil
        }
        statusLabel.stringValue = text ?? ""
        statusLabel.isHidden = text == nil
    }

    /// Why Search can't open, from the index's own state (spec/08), or
    /// why its coverage can't be read either.
    private static func unavailableText(_ coverage: SearchCoverage?, failure: (any Error)?) -> String {
        guard let coverage else {
            return failure.map { "Search is unavailable: \($0.localizedDescription)" } ?? "Search is unavailable"
        }
        if case .blockedNewerExtractor = coverage.indexState {
            return "Search needs a newer version of Sweetbriar. Update the app to search this archive."
        }
        guard !coverage.searchCanOpen else { return "Search is unavailable" }
        if case .pausedByUser = coverage.indexState {
            return "Search is unavailable while the rebuild is paused"
        }
        return "Search is unavailable while the index is rebuilt"
    }

    /// The coverage strip shows while Search results carry a status,
    /// zero hits included, or standalone coverage while Search can't
    /// open (spec/08), and collapses otherwise. The note keeps the
    /// rows' snapshot; the controls follow the newest coverage.
    private func updateCoverage() {
        var note: SearchCoverageNote?
        var shown: SearchCoverage?
        if shownSpec?.isSearch == true {
            if standalone {
                shown = indexControls.coverage
                note = shown.map { SearchCoverageNote(coverage: $0, excludedUnknown: 0) }
            } else if let status = model?.searchStatus {
                note = SearchCoverageNote(status: status)
                shown = indexControls.shown(with: status.coverage)
            }
        }
        if let note, let shown {
            var failure = indexControls.commandError.map {
                "Couldn't update the index: \($0.localizedDescription)"
            }
            if failure == nil, standalone, let read = indexControls.readError {
                failure = "Couldn't read the index's coverage: \(read.localizedDescription)"
            }
            coverage.show(
                note,
                offered: IndexControls(coverage: shown),
                busy: indexControls.pending != nil,
                failure: failure
            )
        }
        coverage.isHidden = note == nil
        coverageCollapsed.isActive = note == nil
    }
}

private extension WindowSpec {
    var isSearch: Bool {
        if case .search = self { return true }
        return false
    }
}
