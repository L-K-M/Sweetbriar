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
import QuartzCore
import SwiftUI
import SweetbriarBridge

@MainActor
final class MessageListViewController: NSViewController {
    private let bridge: CoreBridge
    private let listSource: any ListWindowSource
    private let clockOverride: (any FrameClock)?
    private let writes: ArchiveWriteTasks
    private let canWrite: @MainActor () -> Bool
    private let searchModel: SearchFieldModel
    private let search: SearchFieldController
    private let formatter = MessageRowFormatter()
    private let appearance: AppearanceModel
    private let sidebar: SidebarModel
    /// The welcome's actions: File → Add Proton Account… and Import….
    private let onAddAccount: @MainActor () -> Void
    private let onImport: @MainActor () -> Void
    private var clock: DisplayLinkClock?
    private(set) var model: ListWindowModel?
    private(set) var list: AppKitPrototypeList?
    private var openError: (any Error)?
    private var opening = false

    /// Which `show` owns the list: a later one turns earlier tickets
    /// stale.
    private var tickets = InstallTickets()
    private let openingTasks = ArchiveViewTasks()

    private let statusLabel = NSTextField(labelWithString: "")
    /// The first-run welcome, shown over an empty Unified Inbox while
    /// the archive is still fresh (no account, no imported mail).
    private lazy var welcomeView = NSHostingView(
        rootView: WelcomeEmptyState(onAddAccount: onAddAccount, onImport: onImport)
    )
    /// Whether the welcome is covering the list now; tests read it.
    private(set) var isShowingWelcome = false

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
    /// Replaced lists' handles, closing while the new list opens;
    /// `close()` waits for them.
    private var closing: [UUID: Task<Void, Never>] = [:]

    /// The list's generation after each new index: the sidebar
    /// re-reads its counts when it is newer than what it shows.
    var onGeneration: (@MainActor (UInt64) -> Void)?

    /// The selection, by occurrence id, whenever it changes.
    var onSelectionChange: (@MainActor (Set<Int64>) -> Void)?

    /// A search was saved as a view; the shell selects it (spec/08).
    var onViewSaved: (@MainActor (SavedView) -> Void)?

    /// Measurement hooks are installed before each model opens.
    var onPagesDeliveredForBenchmark: (@MainActor ([Int]) -> Void)?
    var onIndexChangedForBenchmark: (@MainActor (UInt64) -> Void)?
    private(set) var shownSpec: WindowSpec?
    private(set) var choiceSerial = 0
    /// spec/08's launch and `search` spans end when a choice's rows
    /// show; a search choice keeps the field's ticket its span names.
    private let latency: LatencyRecorder
    /// Filing spans waiting for their rows to leave this list.
    private var leaving: [(span: LatencySpan, ids: Set<Int64>)] = []
    private static let leavingLimit = 16
    private var shownSearchTicket: UInt64?
    var failureForBenchmark: (any Error)? { openError ?? model?.lastError }
    /// When the latest choice was shown, its opening page returned from
    /// the core, its first pages landed and its list's open returned, in
    /// the `CACurrentMediaTime` timebase the benchmark stamps input with
    /// (plan M1.6r2e, r4b stage timings).
    private(set) var shownAtForBenchmark: Double?
    private(set) var firstPageAtForBenchmark: Double?
    private(set) var landedAtForBenchmark: Double?
    private(set) var openedAtForBenchmark: Double?
    /// The search field the main window's toolbar shows.
    var searchField: NSSearchField { search.searchField }

    /// Whether the list shows Search results.
    var isShowingSearch: Bool { shownSpec?.isSearch == true }

    /// The shown list's row count once its exact index is in; nil while
    /// it opens or after it failed. The window's subtitle follows it.
    var onCountChange: (@MainActor (Int?) -> Void)?

    /// Escape cleared Search; the window collapses its search item.
    var onSearchCancel: (@MainActor () -> Void)?

    /// Escape cleared Search while the list it returns to was still
    /// opening: that list takes the keyboard once installed.
    private var focusesListOnInstall = false

    /// Search's field and model, for the keystroke benchmark (plan M1.6q2).
    var searchFieldForBenchmark: NSSearchField { search.fieldForBenchmark }
    var searchModelForBenchmark: SearchFieldModel { searchModel }

    init(bridge: CoreBridge, sidebar: SidebarModel, appearance: AppearanceModel,
         listSource: (any ListWindowSource)? = nil, clock: (any FrameClock)? = nil,
         writes: ArchiveWriteTasks? = nil, canWrite: @escaping @MainActor () -> Bool = { true },
         latency: LatencyRecorder = .shared,
         onAddAccount: @escaping @MainActor () -> Void = {},
         onImport: @escaping @MainActor () -> Void = {}) {
        self.bridge = bridge
        self.latency = latency
        self.listSource = listSource ?? bridge
        self.clockOverride = clock
        self.writes = writes ?? ArchiveWriteTasks()
        self.canWrite = canWrite
        self.appearance = appearance
        self.sidebar = sidebar
        self.onAddAccount = onAddAccount
        self.onImport = onImport
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
            self?.show(spec, searchTicket: spec.isSearch ? ticket : nil)
        }
        model.onQueryEdited = { [weak self] ticket in
            self?.latency.begin(.search, replacing: ticket)
        }
        search.onSaveAsView = { [weak self] name in
            try await self?.saveView(name: name) ?? ()
        }
        // Escape in the search field clears it and hands the keyboard
        // back to the list, as Mail does.
        search.onCancel = { [weak self] in self?.searchCancelled() }
    }

    /// Save the search field's current query and sort as a view
    /// (spec/08 Search); a failure reaches the strip's inline error.
    func saveView(name: String) async throws {
        guard let queryJSON = searchModel.queryJSON else {
            throw CoreFailure.invalidInput(
                field: "query", message: "Search for something before saving a view."
            )
        }
        let view = try await sidebar.saveView(
            name: name, queryJSON: queryJSON, sort: searchModel.sort.savedViewSort
        )
        onViewSaved?(view)
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
        statusLabel.font = .preferredFont(forTextStyle: .title3)
        statusLabel.textColor = .secondaryLabelColor
        statusLabel.isHidden = true
        statusLabel.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(statusLabel)
        welcomeView.translatesAutoresizingMaskIntoConstraints = false
        welcomeView.isHidden = true
        content.addSubview(welcomeView)
        coverage.isHidden = true
        coverage.translatesAutoresizingMaskIntoConstraints = false
        content.addSubview(coverage)
        NSLayoutConstraint.activate([
            coverage.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            coverage.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            coverage.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            coverageCollapsed,
            // Where content runs under a title bar the field must sit
            // below it to take clicks.
            search.view.topAnchor.constraint(equalTo: content.safeAreaLayoutGuide.topAnchor),
            search.view.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            search.view.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            statusLabel.centerXAnchor.constraint(equalTo: content.centerXAnchor),
            statusLabel.centerYAnchor.constraint(equalTo: content.centerYAnchor),
            statusLabel.widthAnchor.constraint(
                lessThanOrEqualTo: content.widthAnchor, constant: -40
            ),
            // The welcome fills the list area under the search strip.
            welcomeView.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            welcomeView.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            welcomeView.topAnchor.constraint(equalTo: search.view.bottomAnchor),
            welcomeView.bottomAnchor.constraint(equalTo: content.bottomAnchor),
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

    /// The context menu's commands for the rows it targets, given to
    /// each list as it is installed.
    var contextActions: (@MainActor (Set<Int64>) -> [MessageContextAction])? {
        didSet { list?.contextActions = contextActions }
    }

    /// Opens a row's message in its own window, given to each list as
    /// it is installed (spec/08 "Message window").
    var onOpenMessage: (@MainActor (Int64) -> Void)? {
        didSet { list?.onOpen = onOpenMessage }
    }

    /// Space and ⇧Space in the list page the reader.
    var onPageReader: (@MainActor (ReaderPage) -> Void)? {
        didSet { list?.onPage = onPageReader }
    }

    /// The filing undo each installed list answers Edit → Undo with.
    var filingUndoManager: UndoManager? {
        didSet { list?.filingUndoManager = filingUndoManager }
    }

    /// List `spec`: close the current model, then open a fresh one in
    /// a fresh list. A search the field presented names its ticket.
    func show(_ spec: WindowSpec, searchTicket: UInt64? = nil) {
        // A later choice than Escape's keeps the keyboard where it is.
        focusesListOnInstall = false
        search.navigated(to: spec)
        shownSearchTicket = searchTicket
        standalone = false
        indexControls.stopPolling()
        let ticket = tickets.next()
        shownSpec = spec
        choiceSerial += 1
        shownAtForBenchmark = CACurrentMediaTime()
        firstPageAtForBenchmark = nil
        landedAtForBenchmark = nil
        openedAtForBenchmark = nil
        opening = true
        updateStatus()
        updateCoverage()
        // Take the ticket at the user/field choice, before scheduling can reorder it.
        openingTasks.run { [weak self] in
            await self?.install(spec, ticket: ticket)
        }
    }

    private func searchCancelled() {
        onSearchCancel?()
        // Clearing returns to the list Search replaced, which opens anew.
        if opening {
            focusesListOnInstall = true
        } else {
            list?.focus()
        }
    }

    private func install(_ spec: WindowSpec, ticket: Int) async {
        guard tickets.isCurrent(ticket) else { return }
        // spec/08 closes the replaced handle; M1.6r2e overlaps that with this open.
        detach()

        let clock: any FrameClock
        if let clockOverride {
            clock = clockOverride
        } else {
            let displayClock = self.clock ?? DisplayLinkClock(view: view)
            self.clock = displayClock
            clock = displayClock
        }
        let model = ListWindowModel(source: listSource, spec: spec, clock: clock)
        // Only this choice's model stamps its stages: the replaced one
        // may still answer or deliver until `detach` silences it.
        model.onOpeningPageForBenchmark = { [weak self, weak model] in
            guard let self, let model, self.model === model else { return }
            firstPageAtForBenchmark = CACurrentMediaTime()
        }
        model.onPagesDeliveredForBenchmark = { [weak self, weak model] pages in
            guard let self else { return }
            if landedAtForBenchmark == nil, let model, self.model === model {
                landedAtForBenchmark = CACurrentMediaTime()
                rowsShown()
            }
            onPagesDeliveredForBenchmark?(pages)
        }
        let list = AppKitPrototypeList(
            model: model,
            formatter: formatter,
            accountMarker: spec.showsAccountMarker ? .shown : .hidden,
            appearance: appearance,
            accountLabel: { [weak sidebar] id in
                guard let sidebar else { return id }
                return Self.accountLabel(id, sections: sidebar.sections)
            },
            observeAccountLabels: { [weak sidebar] in _ = sidebar?.sections },
            // spec/08: reopen at Top once the selected message is no
            // longer a hit.
            selectionLoss: spec.isSearch ? .reopenAtTop : .keepPosition
        ) { [weak self] in
            self?.indexDidChange()
        }
        list.onSelectionChange = { [weak self] ids in
            self?.onSelectionChange?(ids)
        }
        list.contextActions = contextActions
        list.onOpen = onOpenMessage
        list.onPage = onPageReader
        list.filingUndoManager = filingUndoManager
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
        if focusesListOnInstall {
            focusesListOnInstall = false
            list.focus()
        }

        observe(model)
        observeGeneration(model)
        do {
            try await model.open()
        } catch {
            guard tickets.isCurrent(ticket) else { return }
            openError = error
        }
        guard tickets.isCurrent(ticket) else { return }
        openedAtForBenchmark = CACurrentMediaTime()
        // An empty list has no pages to land: it shows once open.
        if openError == nil, model.count == 0 { rowsShown() }
        opening = false
        if Self.isUnavailable(openError) {
            enterStandalone()
        }
        updateStatus()
        updateCoverage()
    }

    /// An archive or delete span that ends at the first frame after
    /// none of `ids` is listed (spec/08 "Latency log"). One whose rows
    /// stay listed never ends, and goes with this list.
    func endWhenGone(_ span: LatencySpan?, ids: Set<Int64>) {
        guard let span, !ids.isEmpty, model != nil else { return }
        leaving.append((span, ids))
        // A long run of rows that stayed listed keeps only the newest.
        if leaving.count > Self.leavingLimit { leaving.removeFirst() }
        endLeftSpans()
    }

    /// Each new generation may have taken a filed row out.
    private func observeGeneration(_ model: ListWindowModel) {
        withObservationTracking {
            _ = model.generation
        } onChange: { [weak self, weak model] in
            Task { @MainActor [weak self, weak model] in
                guard let self, let model, self.model === model else { return }
                self.endLeftSpans()
                self.observeGeneration(model)
            }
        }
    }

    private func endLeftSpans() {
        guard let model, let list, !leaving.isEmpty else { return }
        leaving.removeAll { pending in
            guard pending.ids.allSatisfy({ model.index(of: $0) == nil }) else { return false }
            latency.end(pending.span, shownIn: list)
            return true
        }
    }

    /// The current choice's first rows are on screen (spec/08 "Latency
    /// log"): the first ends the launch, a search's ends its span.
    private func rowsShown() {
        guard let list else { return }
        latency.launchShown(in: list)
        if let shownSearchTicket { latency.end(.search, key: shownSearchTicket, shownIn: list) }
    }

    /// Close the list's model; the window is going away.
    func close() async {
        indexControls.stopPolling()
        searchModel.close()
        tickets.close()
        openingTasks.retire()
        let previous = model
        detach()
        // close() marks a model closed before an outstanding open can answer.
        // Then await that open's late release before creating replacement UI.
        await previous?.close()
        await openingTasks.waitForRetirement()
        for task in Array(closing.values) {
            await task.value
        }
    }

    /// Take the list down now and close its handle in the background.
    private func detach() {
        let old = model
        model = nil
        // Rows another list showed can't leave this one.
        leaving.removeAll()
        openError = nil
        list?.removeFromSuperview()
        list = nil
        // The old results' note goes with them.
        updateCoverage()
        guard let old else { return }
        // Its late deliveries must not reach the benchmark's recorder.
        old.onPagesDeliveredForBenchmark = { _ in }
        old.onOpeningPageForBenchmark = {}
        let id = UUID()
        closing[id] = Task { @MainActor [weak self] in
            await old.close()
            self?.closing[id] = nil
        }
    }

    /// A short unique prefix survives truncation when two monograms collide.
    static func accountLabel(_ id: String, sections: [SidebarModel.Section]) -> String {
        let accounts = sections.filter { $0.id != SidebarModel.unifiedSectionID }.sorted { $0.id < $1.id }
        guard let index = accounts.firstIndex(where: { $0.id == id }) else { return id }
        return "\(index + 1) · \(SidebarViewController.accountLabel(accounts[index], among: sections))"
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
        guard !openingTasks.isRetired, canWrite(), let spec = searchSpec else { return }
        let window = standalone ? nil : model?.searchStatus?.coverage
        guard let shown = indexControls.shown(with: window) else { return }
        writes.run(revision: writes.revision) { [indexControls] in
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
        guard let generation = model?.listGeneration else { return }
        onGeneration?(generation)
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
        let welcome = shouldShowWelcome()
        isShowingWelcome = welcome
        welcomeView.isHidden = !welcome
        // The welcome replaces the "No messages" line; an error or a
        // search always keeps its own status.
        statusLabel.isHidden = text == nil || welcome
        let counted = !standalone && openError == nil && model?.lastError == nil
            && model?.hasExactIndex == true
        onCountChange?(counted ? model?.count : nil)
    }

    /// The welcome belongs only over an empty Unified Inbox in a fresh
    /// archive: no account beyond the local one and no imported
    /// mailboxes yet. Any row, error, search or mailbox choice hides it.
    private func shouldShowWelcome() -> Bool {
        guard shownSpec == .unified(.inbox), !standalone, openError == nil,
              model?.lastError == nil, sidebar.lastError == nil,
              model?.hasExactIndex == true, model?.count == 0,
              sidebar.isFreshArchive
        else { return false }
        return true
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
        if case .readOnly = coverage.indexState {
            return "Search is unavailable: the archive became read-only during an index rebuild"
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
