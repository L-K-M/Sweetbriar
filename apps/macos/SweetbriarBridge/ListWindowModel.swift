// `ListWindowModel`: the paging engine behind the message lists
// (spec/08 "Message list"). It holds the window's id index and pages
// in fixed 200-row pages; keeps at most three pages of rows — the
// visible ones pinned, the next page in the scroll direction
// prefetched, the rest going least recently used first; runs at most
// two reads at a time; and delivers every page through the frame
// batcher, at most once per frame. It shows the opening page before
// the index arrives, follows the window's subscription, and holds the
// selection by id. A Search window also carries the status of the
// rows it shows (spec/08 "Search"). Free of AppKit so the bridge's
// Linux tests exercise it directly.
//
//   open ──► first page (provisional prefix) ──► window_ids (exact)
//     │
//     └──► subscription: next ─► invalidated ─► one index fetch at a
//                                resetRequired   time, then the pages
//                                                and the selection

import Observation

/// The window queries a list model needs from the core (spec/07
/// "Queries"). `CoreBridge` adopts it; tests substitute a fake that
/// can gate reads, count what is in flight and answer
/// `resetRequired`.
public protocol ListWindowSource: Sendable {
    /// `open_window`: register a window over `spec` and return its
    /// first page.
    func openWindow(
        spec: WindowSpec,
        anchor: Anchor,
        limit: UInt32
    ) async throws -> ListWindow

    /// `read_window`: the page `anchor` picks out of window `id` at
    /// `generation`.
    func readWindow(
        id: UInt64,
        generation: UInt64,
        anchor: Anchor,
        limit: UInt32
    ) async throws -> ListWindow

    /// `window_ids`: the window's id index and its generation.
    func windowIDs(id: UInt64) async throws -> WindowIDs

    /// `close_window`: release the handle. Uncancellable.
    func closeWindow(id: UInt64) async

    /// `subscribe`: the window's change events, pulled one at a time.
    func windowEvents(id: UInt64) async throws -> any WindowEventSource
}

/// A window's subscription as the model pulls it (spec/07
/// "Subscriptions"): one `next` at a time.
public protocol WindowEventSource: Sendable {
    func next() async throws -> WindowEvent
}

/// The paging state of one list (spec/08 "Message list").
///
/// The model owns a window handle over `spec`, the window's whole id
/// index (`ids`), and a page cache of at most `cachedPageLimit`
/// pages. `setVisible` is the only driver of reads: it pins the pages
/// covering the visible rows, prefetches the next page in the scroll
/// direction, and cancels reads for pages that are no longer wanted.
/// Rows reach observers through a `FrameBatcher`, so a landing page
/// is applied at most once per frame; `row(at:)` answers from the
/// cache only and returns nil for a placeholder.
///
/// Everything here runs on the main actor. Page reads are spawned as
/// tasks that hop to the source and hop back, so `inFlight`'s writes
/// are ordered — a task's bookkeeping entry exists before its body
/// can run.
@MainActor
@Observable
public final class ListWindowModel {
    /// Rows per page (spec/08: fixed 200-row pages).
    public static let pageSize = 200
    /// Search's first page: 50 rows, then the index and 200-row pages
    /// (spec/08 "Search").
    static let searchOpeningLimit = 50
    /// The cache cap (spec/08: at most three pages).
    static let cachedPageLimit = 3
    /// The concurrency cap (spec/08: at most two reads in flight).
    static let inFlightLimit = 2

    /// The generation the id index was taken at. Every page read is
    /// issued at it; `resetRequired` rebuilds the index on the same
    /// handle and re-reads.
    public private(set) var generation: UInt64 = 0

    /// The window's id index: every row's occurrence id in window
    /// order (spec/07). Selection is held by these ids. Until
    /// `hasExactIndex`, only the opening page's ids: a provisional
    /// prefix.
    public private(set) var ids: [Int64] = []

    /// Whether `ids` is the window's whole index rather than the
    /// opening page's prefix: rank jumps and range selection wait for
    /// it (spec/08).
    public private(set) var hasExactIndex = false

    /// The selected rows, by occurrence id (spec/08). After each new
    /// index an id it lacks is dropped — or, in a window of logical
    /// messages, followed to its message's row (spec/07).
    public private(set) var selection: Set<Int64> = []

    /// How many rows the window lists — the list's row count.
    public var count: Int { ids.count }

    /// The last failure a page read or a rebuild produced, if any.
    /// A failed page is retried on the next visibility change; a
    /// `resetRequired` rebuilds instead of landing here.
    public private(set) var lastError: (any Error)?

    /// A Search window's coverage and unknown exclusions, from the
    /// snapshot of the rows it shows (spec/08 "Search"): the oldest
    /// visible page's, so newer counts never sit beside older rows;
    /// with no visible rows, the newest known. Nil for other windows.
    public private(set) var searchStatus: SearchStatus?

    /// Called once per frame batch with the numbers of the pages that
    /// landed in it — the AppKit adapter reloads just those rows.
    @ObservationIgnored
    public var onPagesDelivered: @MainActor ([Int]) -> Void = { _ in }

    /// Part B2b's benchmark hook: it observes the same deliveries as
    /// `onPagesDelivered` — which stays the list's own — so the harness
    /// can stamp each page delivery with the display interval it fell
    /// in without taking over the list's reload.
    @ObservationIgnored
    public var onPagesDeliveredForBenchmark: @MainActor ([Int]) -> Void = { _ in }

    /// Called on the main actor when the id index is replaced —
    /// `open`, `rebuild` and a `resetRequired` rebuild each swap every
    /// row at once, so a list reloads fully rather than patching
    /// pages. Fires before the wanted pages re-request.
    @ObservationIgnored
    public var onIndexChanged: @MainActor () -> Void = {}

    /// Called on the main actor when a new index changed the
    /// selection — not for the list's own `select` calls.
    @ObservationIgnored
    public var onSelectionChanged: @MainActor (Set<Int64>) -> Void = { _ in }

    private let source: any ListWindowSource
    private let spec: WindowSpec
    private let clock: any FrameClock

    // The bookkeeping below is invisible to Observation: only `ids`,
    // `generation`, `lastError` and each `Page`'s rows are observed,
    // so a page landing invalidates only its own rows' views — not
    // every row that once looked its page up (spec/08).

    /// Deliveries queue here and land on the next frame. Created on
    /// first use: the deliver closure needs `self`, so it can't be
    /// built while `init` is still running.
    @ObservationIgnored
    private var batcher: FrameBatcher<PageCompletion>?

    @ObservationIgnored
    private var windowId: UInt64?
    @ObservationIgnored
    private var opening = false
    @ObservationIgnored
    private var closed = false

    /// Every `Page` ever handed out, by page number. Entries are
    /// stable identities for Observation: eviction clears their
    /// `rows`, never the object — a view holding one keeps watching
    /// the same page and sees it turn into a placeholder.
    @ObservationIgnored
    private var pages: [Int: Page] = [:]

    /// Live page reads, page number → (request serial, task). A
    /// cancelled read stays counted until its task finishes, so the
    /// two-in-flight cap covers reads still unwinding too.
    @ObservationIgnored
    private var inFlight: [Int: (serial: UInt64, task: Task<Void, Never>)] = [:]

    /// The most recent request serial per page. A completion naming
    /// an older serial is a superseded request's and is dropped.
    @ObservationIgnored
    private var serials: [Int: UInt64] = [:]
    @ObservationIgnored
    private var nextSerial: UInt64 = 0

    /// Pages whose read finished but whose completion still waits on
    /// a frame, with that read's serial — they count as requested
    /// until that completion is delivered or dropped. Keyed by serial
    /// so a late completion can't clear a newer request's mark.
    @ObservationIgnored
    private var pendingDelivery: [Int: UInt64] = [:]

    /// The handle's release, once `close` started it: a later `close`
    /// awaits it, so "closed" means released whoever asked.
    @ObservationIgnored
    private var closing: Task<Void, Never>?

    /// The generation each loaded page's rows were read at. A page
    /// whose rows are older than `generation` is re-read; its rows
    /// stay shown meanwhile when the new index still puts them there.
    @ObservationIgnored
    private var rowsGeneration: [Int: UInt64] = [:]

    /// Loaded pages holding only a prefix of their rows: a Search
    /// opening page of 50 (spec/08). Re-read in full once the exact
    /// index is in; rows past the prefix show placeholders meanwhile.
    @ObservationIgnored
    private var partialPages: Set<Int> = []

    /// Each Search status received, by the generation its page was
    /// read at — kept for the generations loaded pages still show.
    @ObservationIgnored
    private var statuses: [UInt64: SearchStatus] = [:]

    /// The pull loop over the window's subscription; cancelled by
    /// `close`.
    @ObservationIgnored
    private var subscription: Task<Void, Never>?

    /// Pages whose last read failed (other than `resetRequired`, which
    /// rebuilds). Retried on the next visibility change.
    @ObservationIgnored
    private var failedPages: Set<Int> = []

    /// The pages the current visible range pins plus the prefetch.
    @ObservationIgnored
    private var wanted: Set<Int> = []

    /// `wanted` in request order: visible pages ascending, prefetch last.
    @ObservationIgnored
    private var requestOrder: [Int] = []

    /// The rows the list last reported as visible, as reported: it is
    /// clamped to `ids` only when used, so a range reported while the
    /// index was empty (before a fill) is read once a rebuild brings
    /// rows (spec/08 "Debug menu").
    @ObservationIgnored
    private var visible: Range<Int> = 0 ..< 0

    /// Which way the list last scrolled; `.forward` is toward higher
    /// ranks (older rows). The default before any movement is seen.
    @ObservationIgnored
    private var direction: ScrollDirection = .forward

    /// The last "used" stamp per page — LRU order for eviction.
    @ObservationIgnored
    private var lruStamp: [Int: UInt64] = [:]
    @ObservationIgnored
    private var stamp: UInt64 = 0

    /// id → rank, built on the first `index(of:)` and invalidated
    /// whenever the id index is refetched.
    @ObservationIgnored
    private var reverseIndex: [Int64: Int]?

    /// The one index fetch beside the page reads (spec/08): claimed
    /// before its task starts, held until the selection is reconciled.
    @ObservationIgnored
    private var fetching = false

    /// True while page reads must wait: a fetch awaits the id index
    /// (reads would run at the stale generation), or the selection's
    /// lookups run, which count against the two reads (spec/08).
    @ObservationIgnored
    private var holdingReads = false

    /// `rebuild` calls waiting for the index-fetch slot to go idle.
    @ObservationIgnored
    private var idleWaiters: [CheckedContinuation<Void, Never>] = []

    /// A fetch asked for while one ran: it runs once that one ends, so
    /// an invalidation during a refresh isn't lost (spec/08).
    @ObservationIgnored
    private var pendingFetch: FetchMode?

    /// - Parameters:
    ///   - source: where window queries go — the `CoreBridge`, or a
    ///     fake in tests.
    ///   - spec: which window to list.
    ///   - clock: the display-frame source deliveries are paced by.
    public init(
        source: any ListWindowSource,
        spec: WindowSpec,
        clock: any FrameClock
    ) {
        self.source = source
        self.spec = spec
        self.clock = clock
    }

    /// One page of the window as an observable object. `rows` is nil
    /// until the page lands, so a SwiftUI row view watches only its
    /// own page and a landing invalidates only its rows (spec/08).
    /// Eviction and rebuilds clear `rows` rather than removing the
    /// page — a held reference just reads as a placeholder again.
    @MainActor
    @Observable
    public final class Page {
        /// Which page of the window this is (`rank / pageSize`).
        public let index: Int

        /// The page's rows once landed; nil while a placeholder.
        /// Written only inside the bridge.
        public internal(set) var rows: [ListRow]?

        init(index: Int) {
            self.index = index
        }

        /// The row at `offset` within the page, when loaded.
        public func row(at offset: Int) -> ListRow? {
            guard let rows, offset >= 0, offset < rows.count else { return nil }
            return rows[offset]
        }
    }

    /// The opening page's row limit: one page, or Search's 50.
    private var openingLimit: Int {
        guard case .search = spec else { return Self.pageSize }
        return Self.searchOpeningLimit
    }

    /// How many pages the id index makes.
    private var pageCount: Int {
        (ids.count + Self.pageSize - 1) / Self.pageSize
    }

    /// Rows cached right now — for tests and the cap's own accounting.
    var cachedRowCount: Int {
        pages.values.reduce(0) { $0 + ($1.rows?.count ?? 0) }
    }

    /// Open the window and load its id index (spec/08): `openWindow`
    /// at `top` with a one-page limit — 50 rows for Search — shown at
    /// once as a provisional prefix, then `windowIDs`. The index keeps
    /// the prefix's rows when it shares their generation; a change
    /// landing between the two moves it, and the pages are read again
    /// at the index's. The subscription starts with the window, so no
    /// change is missed. No other page is read here: `setVisible`
    /// drives fetching.
    public func open() async throws {
        guard !closed, !opening, windowId == nil else { return }
        opening = true
        // Defers run last-first: the opening ends, then a fetch asked
        // for meanwhile starts.
        defer {
            runPendingFetch()
            resumeIdleWaiters()
        }
        defer { opening = false }
        let window = try await source.openWindow(
            spec: spec,
            anchor: .top,
            limit: UInt32(openingLimit)
        )
        if closed {
            await source.closeWindow(id: window.windowId)
            throw CancellationError()
        }
        windowId = window.windowId
        showProvisional(window)
        startSubscription(window.windowId)
        let index: WindowIDs
        do {
            index = try await source.windowIDs(id: window.windowId)
        } catch {
            guard !hasExactIndex else { return }
            subscription?.cancel()
            dropAllRows()
            ids = []
            reverseIndex = nil
            onIndexChanged()
            await source.closeWindow(id: window.windowId)
            windowId = nil
            throw error
        }
        let provisional = generation
        guard !closed, install(index) else { return }
        if index.generation == provisional {
            retainMatchingRows()
        } else {
            dropAllRows()
            serials = [:]
            pendingDelivery = [:]
        }
        onIndexChanged()
        await reconcileSelection(window.windowId)
        await refreshEmptyStatus(window.windowId)
        applyVisibility()
    }

    /// The opening page as the window's provisional prefix: its ids
    /// stand in for the index and its rows land as page 0 on the next
    /// frame (spec/08 "Opening a store window").
    private func showProvisional(_ window: ListWindow) {
        generation = window.generation
        ids = window.rows.map(\.occurrenceId)
        reverseIndex = nil
        if let status = window.search {
            statuses[window.generation] = status
        }
        seedFirstPage(from: window)
        publishStatus()
        onIndexChanged()
    }

    /// Install a fetched id index, unless one from a later generation
    /// is already in: a fetch that began before a fill can land after
    /// the fill's rebuild, and must not roll the window back to an
    /// older (perhaps empty) index that no read would ever correct.
    /// Returns whether it was installed.
    private func install(_ index: WindowIDs) -> Bool {
        guard !hasExactIndex || index.generation >= generation else { return false }
        generation = index.generation
        ids = index.ids
        reverseIndex = nil
        hasExactIndex = true
        return true
    }

    /// The rows the list is showing, as window ranks. Pins the pages
    /// covering them, prefetches the next page in the scroll
    /// direction, cancels reads for pages no longer wanted and lets
    /// the rest go least recently used first (spec/08).
    ///
    /// The direction is inferred from the range's top edge: whichever
    /// way it moved is the way the list scrolled, and before any
    /// movement the model looks ahead — toward older rows.
    public func setVisible(_ range: Range<Int>) {
        if range.lowerBound > visible.lowerBound {
            direction = .forward
        } else if range.lowerBound < visible.lowerBound {
            direction = .backward
        }
        visible = range
        applyVisibility()
    }

    /// The observable page covering row `index` — a SwiftUI row view
    /// observes it so a landing page redraws only its own rows.
    /// Never fetches: `setVisible` decides what is read. An
    /// out-of-range index gets an inert, unattached placeholder.
    public func page(containing index: Int) -> Page {
        let k = index / Self.pageSize
        guard index >= 0, index < ids.count else {
            return Page(index: k)
        }
        if let page = pages[k] {
            touch(k)
            return page
        }
        let page = Page(index: k)
        pages[k] = page
        touch(k)
        return page
    }

    /// The cached row at `index`, or nil while its page is out — the
    /// list draws a placeholder of the same height (spec/08). Never
    /// fetches.
    public func row(at index: Int) -> ListRow? {
        guard index >= 0, index < ids.count else { return nil }
        return pages[index / Self.pageSize]?.row(at: index % Self.pageSize)
    }

    /// The occurrence id of the row at `index`, straight from the id
    /// index — a 10k-row selection fetches no rows (spec/08).
    public func occurrenceID(at index: Int) -> Int64? {
        guard index >= 0, index < ids.count else { return nil }
        return ids[index]
    }

    /// The window position of `occurrenceId`, for mapping a selection
    /// by id back to rows. The reverse index is built lazily and
    /// dropped when the id index changes.
    public func index(of occurrenceId: Int64) -> Int? {
        if reverseIndex == nil {
            reverseIndex = Dictionary(
                ids.enumerated().map { ($0.element, $0.offset) },
                uniquingKeysWith: { _, last in last }
            )
        }
        return reverseIndex?[occurrenceId]
    }

    /// Rebuild the model on its existing window (spec/08): cancel
    /// every read, refetch the id index on the same handle, clear the
    /// cache and re-request the wanted pages. `resetRequired` triggers
    /// it internally; the Debug menu calls it after a fill. While the
    /// opening or another fetch holds the one index-fetch slot, the
    /// rebuild runs once it ends, and this returns after it; its
    /// failure is then `lastError`.
    public func rebuild() async throws {
        guard !closed, windowId != nil || opening else { return }
        if fetching || opening {
            pendingFetch = .rebuild
            await withCheckedContinuation { idleWaiters.append($0) }
            return
        }
        guard let wid = windowId else { return }
        fetching = true
        try await fetchIndex(wid, mode: .rebuild)
    }

    /// Hold the selection, by occurrence id (spec/08). The model keeps
    /// it across new indexes; `onSelectionChanged` reports the changes
    /// a new index makes.
    public func select(_ ids: Set<Int64>) {
        selection = ids
    }

    /// The index fetch once `fetching` has been claimed: holds off
    /// `pump` until the fresh index is in, applies visibility so the
    /// wanted pages re-read at the new generation, then reconciles the
    /// selection. A rebuild clears every page; a refresh keeps the
    /// rows the new index still puts where they were, shown until
    /// their re-read lands. A fetch asked for meanwhile runs next.
    private func fetchIndex(_ wid: UInt64, mode: FetchMode) async throws {
        defer {
            fetching = false
            runPendingFetch()
            resumeIdleWaiters()
        }
        guard !closed else { return }
        holdingReads = true
        for (_, entry) in inFlight {
            entry.task.cancel()
        }
        let index: WindowIDs
        do {
            index = try await source.windowIDs(id: wid)
        } catch {
            holdingReads = false
            pump()
            throw error
        }
        guard !closed, install(index) else {
            holdingReads = false
            pump()
            return
        }
        serials = [:]
        pendingDelivery = [:]
        failedPages = []
        switch mode {
        case .rebuild:
            dropAllRows()
        case .refresh:
            retainMatchingRows()
        }
        onIndexChanged()
        await reconcileSelection(wid)
        await refreshEmptyStatus(wid)
        holdingReads = false
        applyVisibility()
    }

    /// Resume `rebuild` callers once the index-fetch slot is idle: no
    /// opening, no fetch, none pending.
    private func resumeIdleWaiters() {
        guard !opening, !fetching, pendingFetch == nil else { return }
        let waiters = idleWaiters
        idleWaiters = []
        for waiter in waiters {
            waiter.resume()
        }
    }

    /// Ask for an index fetch: now if the slot is free, else once the
    /// running fetch — or the opening's — ends.
    private func scheduleFetch(_ mode: FetchMode) {
        guard !closed else { return }
        if fetching || opening {
            pendingFetch = max(pendingFetch ?? mode, mode)
            return
        }
        guard let wid = windowId else { return }
        fetching = true
        Task { [weak self] in
            guard let self else { return }
            do {
                try await self.fetchIndex(wid, mode: mode)
            } catch is CancellationError {
            } catch {
                self.lastError = error
            }
        }
    }

    /// Start the fetch asked for while the slot was busy, if any.
    private func runPendingFetch() {
        guard let mode = pendingFetch else { return }
        pendingFetch = nil
        scheduleFetch(mode)
    }

    /// Pull the window's subscription (spec/07): one `next` at a time.
    /// An invalidation past the installed generation refreshes the
    /// index; `resetRequired` rebuilds. The loop holds the model
    /// weakly and ends with `close`, the window or the core.
    private func startSubscription(_ wid: UInt64) {
        let source = self.source
        subscription = Task { [weak self] in
            do {
                let events = try await source.windowEvents(id: wid)
                while !Task.isCancelled {
                    let event = try await events.next()
                    guard let self else { return }
                    self.handle(event)
                }
            } catch is CancellationError {
            } catch {
                guard let self, !self.closed else { return }
                self.lastError = error
            }
        }
    }

    private func handle(_ event: WindowEvent) {
        switch event {
        case .invalidated(let changed):
            guard !hasExactIndex || changed > generation else { return }
            scheduleFetch(.refresh)
        case .resetRequired:
            scheduleFetch(.rebuild)
        }
    }

    /// After a new index, follow the selection (spec/08): an id the
    /// index lacks is dropped, except in a window of logical messages,
    /// where `Around(id)` with limit 1 finds its message's row first;
    /// only `notFound` drops it there. A lookup that fails otherwise
    /// keeps the id for the next index. Ids the list deselected
    /// meanwhile stay deselected.
    private func reconcileSelection(_ wid: UInt64) async {
        let gen = generation
        let missing = selection.filter { index(of: $0) == nil }
        guard !missing.isEmpty else { return }
        // The lookups are reads too: page reads hold off meanwhile, and
        // those still unwinding finish first, so at most two run.
        let held = holdingReads
        holdingReads = true
        defer { holdingReads = held }
        for entry in inFlight.values {
            await entry.task.value
        }
        // old id → the row that replaces it; nil drops it.
        var replacements: [Int64: Int64?] = [:]
        for id in missing {
            guard spec.listsMessages else {
                replacements.updateValue(nil, forKey: id)
                continue
            }
            do {
                let found = try await source.readWindow(
                    id: wid,
                    generation: gen,
                    anchor: .around(id),
                    limit: 1
                )
                replacements.updateValue(found.rows.first?.occurrenceId, forKey: id)
            } catch let failure as CoreFailure {
                if case .notFound = failure {
                    replacements.updateValue(nil, forKey: id)
                }
            } catch {}
        }
        guard !closed, gen == generation else { return }
        var next = selection
        for (old, replacement) in replacements where next.contains(old) {
            next.remove(old)
            if let new = replacement, index(of: new) != nil {
                next.insert(new)
            }
        }
        guard next != selection else { return }
        selection = next
        onSelectionChanged(next)
    }

    /// A Search window with no rows has no page to carry its status
    /// — a zero-hit search still shows coverage, which a
    /// coverage-only change moves (spec/08). After an index at a
    /// generation without one, a single `Top` read fetches it. Like
    /// the selection's lookups it holds page reads and lets those
    /// still unwinding finish first, so at most two reads run.
    private func refreshEmptyStatus(_ wid: UInt64) async {
        guard case .search = spec, ids.isEmpty, statuses[generation] == nil else { return }
        let gen = generation
        let held = holdingReads
        holdingReads = true
        defer { holdingReads = held }
        for entry in inFlight.values {
            await entry.task.value
        }
        guard !closed, gen == generation else { return }
        let window: ListWindow
        do {
            window = try await source.readWindow(id: wid, generation: gen, anchor: .top, limit: 1)
        } catch is CancellationError {
            return
        } catch {
            guard !closed, gen == generation else { return }
            // A newer generation has its own fetch coming.
            if let failure = error as? CoreFailure, case .resetRequired = failure { return }
            lastError = error
            return
        }
        guard !closed, gen == generation, let status = window.search else { return }
        statuses[gen] = status
        publishStatus()
    }

    /// Publish the status of the rows on screen: the oldest
    /// generation any visible page was read at, so a refresh landing
    /// page by page never pairs newer counts with older rows (spec/08:
    /// counts come from the page's status). With no visible rows — a
    /// zero-hit search, placeholders — the newest status stands.
    /// Statuses no loaded page shows any more are let go.
    private func publishStatus() {
        let shown = (visiblePages.map(Array.init) ?? []).compactMap { rowsGeneration[$0] }
        let newest = statuses.keys.max()
        let next = (shown.min() ?? newest).flatMap { statuses[$0] }
        let kept = Set(rowsGeneration.values).union(newest.map { [$0] } ?? [])
        statuses = statuses.filter { kept.contains($0.key) }
        if next != searchStatus {
            searchStatus = next
        }
    }

    /// Clear every page's rows; the `Page` objects stay.
    private func dropAllRows() {
        for page in pages.values where page.rows != nil {
            page.rows = nil
        }
        rowsGeneration = [:]
        partialPages = []
    }

    /// Keep a page's rows only while the new index puts exactly those
    /// rows there — a partial page, its prefix; the rest become
    /// placeholders. Kept rows read at an older generation, or only a
    /// prefix, are re-read (`needsRead`).
    private func retainMatchingRows() {
        for (k, page) in pages {
            guard let rows = page.rows else { continue }
            let s = k * Self.pageSize
            let L = min(Self.pageSize, ids.count - s)
            let held = partialPages.contains(k) ? min(rows.count, L) : L
            let matches = L > 0
                && rows.count == held
                && rows.map(\.occurrenceId).elementsEqual(ids[s ..< s + held])
            if !matches {
                page.rows = nil
                rowsGeneration[k] = nil
                partialPages.remove(k)
            }
        }
    }

    /// Whether page `k` needs a read: no rows, rows from an older
    /// generation, or only a prefix once the exact index can place
    /// the rest — before it, the prefix is the whole provisional
    /// window.
    private func needsRead(_ k: Int) -> Bool {
        guard pages[k]?.rows != nil else { return true }
        if partialPages.contains(k), hasExactIndex { return true }
        return rowsGeneration[k] != generation
    }

    /// Cancel every read and release the window (spec/07: the shell
    /// closes every window it opens). Uncancellable; a second call is
    /// a no-op.
    public func close() async {
        if let closing {
            await closing.value
            return
        }
        if closed { return }
        closed = true
        subscription?.cancel()
        // No fetch runs after a close: waiting rebuilds return.
        pendingFetch = nil
        let waiters = idleWaiters
        idleWaiters = []
        for waiter in waiters {
            waiter.resume()
        }
        for (_, entry) in inFlight {
            entry.task.cancel()
        }
        guard let wid = windowId else { return }
        let source = self.source
        let release = Task { await source.closeWindow(id: wid) }
        closing = release
        await release.value
    }

    /// Recompute the wanted set from `visible` and `direction`,
    /// cancel reads for pages no longer wanted, then fill the free
    /// read slots (spec/08).
    private func applyVisibility() {
        var newWanted: Set<Int> = []
        var order: [Int] = []
        if let shown = visiblePages {
            for k in shown {
                newWanted.insert(k)
                order.append(k)
            }
            // The prefetch fills the third cache slot: it exists
            // only while the pinned pages leave room.
            let neighbour = direction == .forward ? shown.upperBound + 1 : shown.lowerBound - 1
            if newWanted.count < Self.cachedPageLimit,
               neighbour >= 0, neighbour < pageCount {
                newWanted.insert(neighbour)
                order.append(neighbour)
            }
        }
        wanted = newWanted
        requestOrder = order
        for (page, entry) in inFlight where !wanted.contains(page) {
            entry.task.cancel()
        }
        stamp &+= 1
        for k in wanted {
            lruStamp[k] = stamp
        }
        failedPages = []
        evictOverflow()
        publishStatus()
        pump()
    }

    /// The pages covering the visible rows, clamped to the index; nil
    /// when no row shows.
    private var visiblePages: ClosedRange<Int>? {
        let lo = max(0, min(visible.lowerBound, ids.count))
        let shown = lo ..< max(lo, min(visible.upperBound, ids.count))
        guard !shown.isEmpty else { return nil }
        let first = shown.lowerBound / Self.pageSize
        let last = min((shown.upperBound - 1) / Self.pageSize, pageCount - 1)
        return first <= last ? first ... last : nil
    }

    /// Start reads for wanted pages that need one, visible pages
    /// first, while a read slot is free (spec/08: at most two in
    /// flight). A page already loaded, already reading, delivered but
    /// not yet landed, or failed until the next visibility change is
    /// skipped.
    private func pump() {
        guard !closed, !holdingReads, windowId != nil else {
            return
        }
        for k in requestOrder where inFlight.count < Self.inFlightLimit {
            guard wanted.contains(k),
                  inFlight[k] == nil,
                  pendingDelivery[k] == nil,
                  !failedPages.contains(k),
                  needsRead(k)
            else { continue }
            startRead(k)
        }
    }

    /// Issue the spec/08 page read: page `k` starts at `s = 200k`,
    /// holds `L = min(200, N − s)` rows, and is read
    /// `around(ids[s + ⌊L/2⌋])` with limit `L`.
    private func startRead(_ k: Int) {
        guard let wid = windowId else { return }
        let s = k * Self.pageSize
        let L = min(Self.pageSize, ids.count - s)
        guard L > 0, s + L / 2 < ids.count else { return }
        let gen = generation
        let centerId = ids[s + L / 2]
        nextSerial &+= 1
        serials[k] = nextSerial
        let serial = nextSerial
        // Signpost "queue" (plan M0.10): the read's request →
        // completion span.
        let span = ListPrototypeSignposts.begin("queue")
        let task = Task { [weak self] in
            guard let self else {
                ListPrototypeSignposts.end("queue", span)
                return
            }
            let result: Result<ListWindow, any Error>
            do {
                result = .success(
                    try await self.source.readWindow(
                        id: wid,
                        generation: gen,
                        anchor: .around(centerId),
                        limit: UInt32(L)
                    )
                )
            } catch {
                result = .failure(error)
            }
            ListPrototypeSignposts.end("queue", span)
            self.readFinished(
                page: k,
                serial: serial,
                generation: gen,
                result: result
            )
        }
        inFlight[k] = (serial: serial, task: task)
    }

    /// A read task finished: free its slot (only if the entry is
    /// still this read's — a rebuild may have replaced it), queue the
    /// completion for the next frame, and see whether another page
    /// can start.
    private func readFinished(
        page k: Int,
        serial: UInt64,
        generation gen: UInt64,
        result: Result<ListWindow, any Error>
    ) {
        if inFlight[k]?.serial == serial {
            inFlight[k] = nil
        }
        if !closed {
            queue(PageCompletion(
                page: k,
                serial: serial,
                generation: gen,
                origin: .read,
                result: result
            ))
        }
        pump()
    }

    /// Mark a completion's page pending under its serial and hand it
    /// to the frame batcher, which lands it on the next frame.
    private func queue(_ completion: PageCompletion) {
        pendingDelivery[completion.page] = completion.serial
        if batcher == nil {
            batcher = FrameBatcher(clock: clock) { [weak self] batch in
                self?.deliver(batch)
            }
        }
        batcher?.post(completion)
    }

    /// `open_window`'s own page — `Top` with the page limit — is page
    /// 0 exactly (ranks 0..<min(200, N)), so it lands like a read of
    /// page 0 would, when taken at the installed generation: the first
    /// rows cost no second round trip. Search's 50-row page lands as
    /// page 0's prefix instead. Not wanted by then, it's dropped like
    /// any late read.
    private func seedFirstPage(from window: ListWindow) {
        guard window.generation == generation, !window.rows.isEmpty else {
            return
        }
        nextSerial &+= 1
        serials[0] = nextSerial
        queue(PageCompletion(
            page: 0,
            serial: nextSerial,
            generation: generation,
            origin: .opening,
            result: .success(window)
        ))
    }

    /// Apply one frame's worth of read completions (spec/08: pages
    /// land at most once per frame). A completion from an older
    /// generation, a superseded request, or a page no longer wanted
    /// is dropped. A landed page whose ids aren't exactly
    /// `ids[s..<s+L]` — or, for a short opening page, their prefix —
    /// and any `resetRequired`, rebuilds the model.
    private func deliver(_ batch: [PageCompletion]) {
        // Signpost "apply" (plan M0.10): the frame's delivery handling —
        // landing rows plus the lists' reload work below.
        let span = ListPrototypeSignposts.begin("apply")
        defer { ListPrototypeSignposts.end("apply", span) }
        var landed: [Int] = []
        var reset = false
        for completion in batch {
            if pendingDelivery[completion.page] == completion.serial {
                pendingDelivery[completion.page] = nil
            }
            if closed || reset { continue }
            guard completion.generation == generation,
                  serials[completion.page] == completion.serial,
                  wanted.contains(completion.page)
            else { continue }
            switch completion.result {
            case .success(let window):
                let s = completion.page * Self.pageSize
                let L = min(Self.pageSize, ids.count - s)
                // An opening page cut short by its limit holds page
                // 0's prefix; whichever lands first, it or the index.
                let partial = completion.origin == .opening
                    && window.hasMoreAfter
                    && window.rows.count < Self.pageSize
                let held = partial ? min(window.rows.count, L) : L
                guard s >= 0,
                      L > 0,
                      s + L <= ids.count,
                      window.rows.count == held,
                      window.rows.map(\.occurrenceId)
                          .elementsEqual(ids[s ..< s + held])
                else {
                    reset = true
                    continue
                }
                let page = pages[completion.page] ?? Page(index: completion.page)
                pages[completion.page] = page
                page.rows = window.rows
                rowsGeneration[completion.page] = completion.generation
                if partial {
                    partialPages.insert(completion.page)
                } else {
                    partialPages.remove(completion.page)
                }
                if let status = window.search {
                    statuses[completion.generation] = status
                }
                stamp &+= 1
                lruStamp[completion.page] = stamp
                landed.append(completion.page)
            case .failure(let error):
                if error is CancellationError { continue }
                if let failure = error as? CoreFailure,
                   case .resetRequired = failure {
                    reset = true
                    continue
                }
                failedPages.insert(completion.page)
                lastError = error
            }
        }
        if !landed.isEmpty {
            // The benchmark stamps the delivery first, so a list's own
            // synchronous work in its hook (AppKit's reload) counts
            // inside the delivery's latency, as SwiftUI's deferred
            // update does.
            onPagesDeliveredForBenchmark(landed)
            onPagesDelivered(landed)
        }
        // A fetch already running installs a fresh index anyway: the
        // reads that failed were issued before it began.
        if reset, !fetching {
            scheduleFetch(.rebuild)
        }
        evictOverflow()
        publishStatus()
        pump()
    }

    /// Pages holding rows beyond the cap go least recently used
    /// first — but never a wanted one (spec/08: the visible pages are
    /// pinned). Eviction clears `rows` and keeps the `Page` object,
    /// so a view holding it sees a placeholder, not a stale row.
    private func evictOverflow() {
        var excess = pages.values.reduce(0) { $0 + ($1.rows == nil ? 0 : 1) }
            - Self.cachedPageLimit
        guard excess > 0 else { return }
        let evictable = pages.keys
            .filter { !wanted.contains($0) && pages[$0]?.rows != nil }
            .sorted { (lruStamp[$0] ?? 0) < (lruStamp[$1] ?? 0) }
        for k in evictable {
            guard excess > 0 else { return }
            pages[k]?.rows = nil
            rowsGeneration[k] = nil
            partialPages.remove(k)
            lruStamp[k] = nil
            excess -= 1
        }
    }

    /// Mark a page as just used, for LRU eviction.
    private func touch(_ k: Int) {
        stamp &+= 1
        lruStamp[k] = stamp
    }

    /// What an index fetch does with the loaded pages, weakest first:
    /// a pending fetch takes the stronger of two asks.
    private enum FetchMode: Comparable {
        /// Keep the rows the new index still puts where they were.
        case refresh
        /// Clear every page.
        case rebuild
    }

    /// Which way the visible range last moved.
    private enum ScrollDirection {
        /// Toward higher ranks — older rows.
        case forward
        /// Toward rank zero — newer rows.
        case backward
    }

    /// One finished read waiting for, or travelling through, the
    /// frame batch.
    private struct PageCompletion: Sendable {
        var page: Int
        var serial: UInt64
        var generation: UInt64
        var origin: Origin
        var result: Result<ListWindow, any Error>

        /// Which request a completion answers.
        enum Origin {
            /// `open_window`'s first page, at the spec's opening limit.
            case opening
            /// A spec/08 page read.
            case read
        }
    }
}

extension WindowSpec {
    /// Whether the window lists logical messages, one row each, rather
    /// than occurrences (spec/07 grain).
    var listsMessages: Bool {
        switch self {
        case .flagged, .thread, .search:
            return true
        case .debugSynthetic, .mailbox, .unified:
            return false
        }
    }
}
