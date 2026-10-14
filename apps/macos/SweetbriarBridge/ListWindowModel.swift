// `ListWindowModel`: the paging engine behind both prototype lists
// (spec/08 "Message list"). It holds the window's id index and pages
// in fixed 200-row pages; keeps at most three pages of rows — the
// visible ones pinned, the next page in the scroll direction
// prefetched, the rest going least recently used first; runs at most
// two reads at a time; and delivers every page through the frame
// batcher, at most once per frame. Free of AppKit so the bridge's
// Linux tests exercise it directly.

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
    /// The cache cap (spec/08: at most three pages).
    static let cachedPageLimit = 3
    /// The concurrency cap (spec/08: at most two reads in flight).
    static let inFlightLimit = 2

    /// The generation the id index was taken at. Every page read is
    /// issued at it; `resetRequired` rebuilds the index on the same
    /// handle and re-reads.
    public private(set) var generation: UInt64 = 0

    /// The window's id index: every row's occurrence id in window
    /// order (spec/07). Selection is held by these ids.
    public private(set) var ids: [Int64] = []

    /// How many rows the window lists — the list's row count.
    public var count: Int { ids.count }

    /// The last failure a page read or a rebuild produced, if any.
    /// A failed page is retried on the next visibility change; a
    /// `resetRequired` rebuilds instead of landing here.
    public private(set) var lastError: (any Error)?

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

    /// Whether an id index is installed — a failed opening fetch then
    /// leaves the window to the rebuild that installed it.
    @ObservationIgnored
    private var hasIndex = false

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

    /// True while `rebuild` awaits the id index: reads issued then
    /// would run at the stale generation, so `pump` holds off. Also
    /// the dedup flag for concurrent `resetRequired` triggers —
    /// `scheduleRebuild` claims it before its task starts.
    @ObservationIgnored
    private var rebuilding = false

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

    /// How many pages the id index makes.
    private var pageCount: Int {
        (ids.count + Self.pageSize - 1) / Self.pageSize
    }

    /// Rows cached right now — for tests and the cap's own accounting.
    var cachedRowCount: Int {
        pages.values.reduce(0) { $0 + ($1.rows?.count ?? 0) }
    }

    /// Open the window and load its id index (spec/08): `openWindow`
    /// at `top` with a one-page limit, then `windowIDs`. A fill
    /// landing between the two moves the generation — the fresh index
    /// is the same answer a rebuild would refetch, so it is adopted.
    /// No page is read here: `setVisible` drives fetching.
    public func open() async throws {
        guard !closed, !opening, windowId == nil else { return }
        opening = true
        defer { opening = false }
        let window = try await source.openWindow(
            spec: spec,
            anchor: .top,
            limit: UInt32(Self.pageSize)
        )
        if closed {
            await source.closeWindow(id: window.windowId)
            throw CancellationError()
        }
        windowId = window.windowId
        let index: WindowIDs
        do {
            index = try await source.windowIDs(id: window.windowId)
        } catch {
            guard !hasIndex else { return }
            await source.closeWindow(id: window.windowId)
            windowId = nil
            throw error
        }
        guard !closed, install(index) else { return }
        onIndexChanged()
        applyVisibility()
    }

    /// Install a fetched id index, unless one from a later generation
    /// is already in: a fetch that began before a fill can land after
    /// the fill's rebuild, and must not roll the window back to an
    /// older (perhaps empty) index that no read would ever correct.
    /// Returns whether it was installed.
    private func install(_ index: WindowIDs) -> Bool {
        guard !hasIndex || index.generation >= generation else { return false }
        generation = index.generation
        ids = index.ids
        reverseIndex = nil
        hasIndex = true
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
    /// it internally; the Debug menu calls it after a fill. A second
    /// call while one rebuild is running does nothing — the running
    /// one refetches a fresh index anyway.
    public func rebuild() async throws {
        guard !closed, !rebuilding, let wid = windowId else { return }
        try await rebuildNow(wid)
    }

    /// The rebuild body once `rebuilding` has been claimed: holds off
    /// `pump` until the fresh index is in, then applies visibility so
    /// the wanted pages re-read at the new generation.
    private func rebuildNow(_ wid: UInt64) async throws {
        guard !closed else {
            rebuilding = false
            return
        }
        rebuilding = true
        for (_, entry) in inFlight {
            entry.task.cancel()
        }
        let index: WindowIDs
        do {
            index = try await source.windowIDs(id: wid)
        } catch {
            rebuilding = false
            throw error
        }
        rebuilding = false
        guard !closed, install(index) else { return }
        serials = [:]
        pendingDelivery = [:]
        failedPages = []
        for page in pages.values where page.rows != nil {
            page.rows = nil
        }
        onIndexChanged()
        applyVisibility()
    }

    /// Cancel every read and release the window (spec/07: the shell
    /// closes every window it opens). Uncancellable; a second call is
    /// a no-op.
    public func close() async {
        if closed { return }
        closed = true
        for (_, entry) in inFlight {
            entry.task.cancel()
        }
        guard let wid = windowId else { return }
        await source.closeWindow(id: wid)
    }

    /// Recompute the wanted set from `visible` and `direction`,
    /// cancel reads for pages no longer wanted, then fill the free
    /// read slots (spec/08).
    private func applyVisibility() {
        var newWanted: Set<Int> = []
        var order: [Int] = []
        let lo = max(0, min(visible.lowerBound, ids.count))
        let shown = lo ..< max(lo, min(visible.upperBound, ids.count))
        if !shown.isEmpty {
            let first = shown.lowerBound / Self.pageSize
            let last = min((shown.upperBound - 1) / Self.pageSize, pageCount - 1)
            if first <= last {
                for k in first ... last {
                    newWanted.insert(k)
                    order.append(k)
                }
                // The prefetch fills the third cache slot: it exists
                // only while the pinned pages leave room.
                let neighbour = direction == .forward ? last + 1 : first - 1
                if newWanted.count < Self.cachedPageLimit,
                   neighbour >= 0, neighbour < pageCount {
                    newWanted.insert(neighbour)
                    order.append(neighbour)
                }
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
        pump()
    }

    /// Start reads for wanted pages that need one, visible pages
    /// first, while a read slot is free (spec/08: at most two in
    /// flight). A page already loaded, already reading, delivered but
    /// not yet landed, or failed until the next visibility change is
    /// skipped.
    private func pump() {
        guard !closed, !rebuilding, windowId != nil else {
            return
        }
        for k in requestOrder where inFlight.count < Self.inFlightLimit {
            guard wanted.contains(k),
                  inFlight[k] == nil,
                  pendingDelivery[k] == nil,
                  !failedPages.contains(k),
                  pages[k]?.rows == nil
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
            pendingDelivery[k] = serial
            if batcher == nil {
                batcher = FrameBatcher(clock: clock) { [weak self] batch in
                    self?.deliver(batch)
                }
            }
            batcher?.post(
                PageCompletion(
                    page: k,
                    serial: serial,
                    generation: gen,
                    result: result
                )
            )
        }
        pump()
    }

    /// Apply one frame's worth of read completions (spec/08: pages
    /// land at most once per frame). A completion from an older
    /// generation, a superseded request, or a page no longer wanted
    /// is dropped. A landed page whose ids aren't exactly
    /// `ids[s..<s+L]`, and any `resetRequired`, rebuilds the model.
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
                guard s >= 0,
                      L > 0,
                      s + L <= ids.count,
                      window.rows.count == L,
                      window.rows.map(\.occurrenceId)
                          .elementsEqual(ids[s ..< s + L])
                else {
                    reset = true
                    continue
                }
                let page = pages[completion.page] ?? Page(index: completion.page)
                pages[completion.page] = page
                page.rows = window.rows
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
            onPagesDelivered(landed)
            onPagesDeliveredForBenchmark(landed)
        }
        if reset {
            scheduleRebuild()
        }
        evictOverflow()
        pump()
    }

    /// Kick off a rebuild from inside a delivery. `rebuilding` claims
    /// the slot before the task starts, so concurrent triggers
    /// collapse into it and `pump` holds off — it refetches a fresh
    /// index either way.
    private func scheduleRebuild() {
        guard !closed, !rebuilding, let wid = windowId else { return }
        rebuilding = true
        Task { [weak self] in
            guard let self else { return }
            do {
                try await self.rebuildNow(wid)
            } catch {
                self.lastError = error
            }
        }
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
            lruStamp[k] = nil
            excess -= 1
        }
    }

    /// Mark a page as just used, for LRU eviction.
    private func touch(_ k: Int) {
        stamp &+= 1
        lruStamp[k] = stamp
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
        var result: Result<ListWindow, any Error>
    }
}
