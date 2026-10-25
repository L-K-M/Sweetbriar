// The main window's message list (spec/08 "Message list"): the AppKit
// list over a `ListWindowModel` for the window the sidebar chose.
// Showing another window closes the old model first, and a later
// choice wins over one still opening. A status line covers the list
// while it opens, when it is empty, and when it failed; Search results
// carry a coverage strip below them.

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
            search.view.topAnchor.constraint(equalTo: content.topAnchor),
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

    /// List `spec`: close the current model, then open a fresh one in
    /// a fresh list.
    func show(_ spec: WindowSpec) {
        search.navigated(to: spec)
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
        updateStatus()
        updateCoverage()
    }

    /// Close the list's model; the window is going away.
    func close() async {
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
                self.updateStatus()
                self.updateCoverage()
                self.observe(model)
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
            text = shownSpec?.isSearch == true ? "No matching messages" : "No messages"
        } else {
            text = nil
        }
        statusLabel.stringValue = text ?? ""
        statusLabel.isHidden = text == nil
    }

    /// The coverage strip shows while Search results carry a status,
    /// zero hits included (spec/08), and collapses otherwise.
    private func updateCoverage() {
        let status = shownSpec?.isSearch == true ? model?.searchStatus : nil
        if let status {
            coverage.show(SearchCoverageNote(status: status))
        }
        coverage.isHidden = status == nil
        coverageCollapsed.isActive = status == nil
    }
}

private extension WindowSpec {
    var isSearch: Bool {
        if case .search = self { return true }
        return false
    }
}
