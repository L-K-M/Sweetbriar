// The list benchmark harness (plan M0.10, spec/08 "Debug menu"): the
// four ABBA runs — AppKit cold, SwiftUI cold, SwiftUI warm, AppKit
// warm — each in a fresh List Prototype–style window and model over
// the same 500,000-row fill. Jumps go through the seam's
// `scrollToRank`, wheel and mouse input arrive as posted events
// identical for both lists, and a continuously running display link
// supplies the intervals CPU, latency and hitches are measured over.
// A check that fails or times out fails its run; a generation change
// aborts the whole benchmark.

import AppKit
import SweetbriarBridge

/// One event posted for the record, or resolved on a tick.
private struct RecordedEvent {
    var phase: String
    var detail: String
    /// `CACurrentMediaTime()` at post.
    var postedAt: Double
    /// The expected-state predicate — the event is render-ready at the
    /// first tick it holds.
    var expected: (@MainActor () -> Bool)?
    /// When a run-loop commit point first passed with the expected
    /// state holding: the state was laid out and committed then.
    var committedAt: Double?
    /// The first tick after `committedAt` — the frame presenting it.
    var renderReadyTick: Int?
}

/// One run's live measurement state: the sampler's tick log and the
/// events posted so far, with render-ready evaluated per tick.
@MainActor
private final class RunContext {
    let sampler = BenchmarkTickSampler()
    private(set) var events: [RecordedEvent] = []
    /// A tick saw the model leave the fill's generation — the run
    /// must stop (plan M0.10: abort the whole run).
    private(set) var generationChanged = false

    /// When the run must stop, whatever it is doing: its time budget.
    let deadline: ContinuousClock.Instant

    init(deadline: ContinuousClock.Instant) {
        self.deadline = deadline
    }

    /// The run overran its budget.
    var overBudget: Bool {
        ContinuousClock.now >= deadline
    }

    /// The run must post no further input: another generation, or no
    /// time left.
    var stopped: Bool {
        generationChanged || overBudget
    }

    /// Flag the abort from outside a tick — an open that landed on
    /// another generation counts too.
    func markGenerationChanged() {
        generationChanged = true
    }

    /// Record one posted input or page delivery.
    /// - Returns: the event's index, for resolving it later.
    @discardableResult
    func record(
        phase: String,
        detail: String,
        postedAt: Double,
        expected: (@MainActor () -> Bool)? = nil
    ) -> Int {
        events.append(RecordedEvent(
            phase: phase,
            detail: detail,
            postedAt: postedAt,
            expected: expected,
            committedAt: nil,
            renderReadyTick: nil
        ))
        return events.count - 1
    }

    /// The main run loop passed its commit point — after AppKit's
    /// display cycle and Core Animation's commit, where both lists'
    /// updates go out. An unresolved event whose expected state holds
    /// here has been laid out and committed. (Testing for pending
    /// layout at a tick instead fails under 60 Hz input: every tick
    /// sees the views the newest event just dirtied.)
    func commitPointPassed() {
        let now = CACurrentMediaTime()
        for i in events.indices where events[i].committedAt == nil {
            guard let expected = events[i].expected,
                  now >= events[i].postedAt,
                  expected()
            else {
                continue
            }
            events[i].committedAt = now
        }
    }

    /// A tick landed: flag a generation change, then resolve every
    /// event committed before it — the first tick after its commit is
    /// its render-ready (plan M0.10: "the first tick after which its
    /// rows are laid out and visible").
    func tickDidFire(_ index: Int) {
        let ticks = sampler.ticks
        guard index < ticks.count else { return }
        if ticks[index].generationChanged {
            generationChanged = true
        }
        let firedAt = ticks[index].firedAt
        for i in events.indices where events[i].renderReadyTick == nil {
            guard let committedAt = events[i].committedAt,
                  firedAt > committedAt
            else {
                continue
            }
            events[i].renderReadyTick = index
        }
    }
}

@MainActor
final class ListBenchmarkRunner {
    /// The ABBA order (plan M0.10): cold is the implementation's first
    /// run in the process, warm its second.
    private static let sequence:
        [(PrototypeImplementation, BenchmarkRun.Pass)] = [
            (.appkit, .cold),
            (.swiftui, .cold),
            (.swiftui, .warm),
            (.appkit, .warm),
        ]

    /// Each run's time budget: past it, the run posts no more input
    /// and fails, so a list that stalls for minutes (SwiftUI over
    /// 500k ids can) still leaves a report. Checked between inputs —
    /// a blocked main thread overruns it by the length of the block.
    private static let runBudget: Duration = .seconds(300)

    private let session: CoreSession
    private let clock = ContinuousClock()
    /// When the benchmark started, for the progress lines.
    private let startedAt = ContinuousClock.now
    /// Non-nil once a model has seen another generation — every
    /// remaining run is skipped.
    private var generationAbort: String?

    init(session: CoreSession) {
        self.session = session
    }

    /// The whole benchmark: fill, four runs, the report. Never
    /// throws — a run that can't start reports `ok: false`.
    func run() async -> ListBenchmarkReport {
        var environment = BenchmarkEnvironment.collect()
        var runs: [BenchmarkRun] = []
        var failures: [String] = []
        do {
            let bridge = try await session.bridge()
            environment.baselineFootprintBytes =
                SystemProbes.physicalFootprint()
            let generation = try await bridge.debugFillSynthetic(
                count: MainMenuBuilder.syntheticFillCount
            )
            environment.rowCount = Int(MainMenuBuilder.syntheticFillCount)
            log("filled generation \(generation)")
            for (implementation, pass) in Self.sequence
            where generationAbort == nil {
                await settle()
                runs.append(await runOnce(
                    implementation: implementation,
                    pass: pass,
                    generation: generation
                ))
            }
            if let generationAbort {
                failures.append(generationAbort)
            }
        } catch {
            failures.append(
                "the benchmark couldn't start: \(error.localizedDescription)"
            )
        }
        for run in runs {
            failures.append(contentsOf: run.failures.map {
                "\(run.implementation) \(run.pass.rawValue): \($0)"
            })
        }
        return ListBenchmarkReport(
            ok: failures.isEmpty,
            failures: failures,
            environment: environment,
            runs: runs,
            comparison: BenchmarkComparison.evaluate(runs: runs)
        )
    }

    // MARK: - One run

    /// One implementation, one pass: open the window and model, run
    /// the jump, wheel and selection phases with their checks, close
    /// both, and reduce the tick log and events to the run's report.
    private func runOnce(
        implementation: PrototypeImplementation,
        pass: BenchmarkRun.Pass,
        generation: UInt64
    ) async -> BenchmarkRun {
        var failures: [String] = []
        let baseline = SystemProbes.physicalFootprint()
        let controller = ListPrototypeWindowController(
            session: session,
            implementation: implementation
        )
        guard let window = controller.window, let content = window.contentView
        else {
            return failedRun(
                implementation, pass, generation,
                failures: ["the prototype window couldn't be created"],
                baseline: baseline
            )
        }

        // "makes it key and front" — the runs are comparable only
        // while the window is key and the app active, so record both.
        NSApp.activate()
        controller.showWindow(nil)
        window.makeKeyAndOrderFront(nil)
        let windowWasKey = window.isKeyWindow
        let appWasActive = NSApp.isActive
        let openedAt = CACurrentMediaTime()
        // The screen answers the nominal interval — read it before
        // the window's death can nil it.
        let screen = window.screen

        // The run's display link runs continuously — separate from
        // the model's pausing clock.
        let context = RunContext(deadline: clock.now + Self.runBudget)
        log("\(implementation.rawValue) \(pass.rawValue): opening")
        context.sampler.extras = { [weak controller] in
            guard let list = controller?.prototypeList,
                  let model = controller?.listModel
            else {
                return .init()
            }
            return .init(
                scrollPosition: Self.scrollPosition(of: list),
                visibleRowsPopulated: Self.allVisibleRowsPopulated(
                    list: list, model: model
                ),
                // Generation 0 is a model still opening: the fill
                // made the source's generation at least 1.
                generationChanged: model.generation != 0
                    && model.generation != generation
            )
        }
        context.sampler.onTick = { [weak context] index in
            context?.tickDidFire(index)
        }
        context.sampler.onCommitPoint = { [weak context] in
            context?.commitPointPassed()
        }
        context.sampler.start(on: content)

        // Time to first populated rows: window opening → render-ready
        // of the first visible rows.
        context.record(
            phase: "firstRows",
            detail: "window opened",
            postedAt: openedAt
        ) { [weak controller] in
            guard let list = controller?.prototypeList,
                  let model = controller?.listModel
            else {
                return false
            }
            return Self.allVisibleRowsPopulated(list: list, model: model)
        }

        controller.openList()
        if let openFailure = await waitForOpen(
            controller, generation: generation
        ) {
            failures.append(openFailure)
        }
        log("\(implementation.rawValue) \(pass.rawValue): opened")
        // Page deliveries are events too — the model's separate hook,
        // so the list's own `onPagesDelivered` stays its own.
        controller.listModel?.onPagesDeliveredForBenchmark = {
            [weak context, weak controller] pages in
            context?.record(
                phase: "pageDeliveries",
                detail: "pages \(pages)",
                postedAt: CACurrentMediaTime()
            ) { [weak controller] in
                guard let list = controller?.prototypeList,
                      let model = controller?.listModel
                else {
                    return false
                }
                // Its rows: the delivered pages' visible ones, laid out —
                // not every visible row, which may wait on other pages.
                return Self.deliveredRowsShown(pages, list: list, model: model)
                }
        }

        let list = controller.prototypeList
        let model = controller.listModel
        if let model, model.generation != 0, model.generation != generation {
            context.markGenerationChanged()
        }
        if failures.isEmpty, let list, let model {
            failures.append(contentsOf: await runJumpPhase(
                context: context, list: list, model: model
            ))
            log("\(implementation.rawValue) \(pass.rawValue): jumps done")
            if !context.stopped {
                failures.append(contentsOf: await runWheelPhase(
                    context: context,
                    window: window,
                    list: list,
                    model: model
                ))
            }
            log("\(implementation.rawValue) \(pass.rawValue): wheel done")
            if !context.stopped {
                failures.append(contentsOf: await runSelectionPhase(
                    context: context,
                    window: window,
                    list: list,
                    model: model
                ))
            }
        }
        log("\(implementation.rawValue) \(pass.rawValue): phases done")
        if context.overBudget {
            failures.append(
                "the run overran its \(Self.runBudget.components.seconds) s budget"
            )
        }
        if context.generationChanged {
            generationAbort =
                "\(implementation.rawValue) \(pass.rawValue): a model "
                + "saw another generation mid-run"
        }

        await drainRenderReady(context: context)
        context.sampler.stop()
        // "closes both after" — the window and its model.
        await controller.closeAndAwaitModel()
        return assembleRun(
            context: context,
            implementation: implementation,
            pass: pass,
            generation: generation,
            failures: failures,
            windowWasKey: windowWasKey,
            appWasActive: appWasActive,
            baseline: baseline,
            screen: screen
        )
    }

    /// Wait for the model to open at the fill's generation; nil on
    /// success, the failure's reason otherwise.
    private func waitForOpen(
        _ controller: ListPrototypeWindowController,
        generation: UInt64
    ) async -> String? {
        // The state is read before the deadline: a list that blocks
        // the main thread past it (SwiftUI over 500k ids can) has
        // opened by the time this loop runs again, and is measured.
        let deadline = clock.now + .seconds(30)
        while true {
            if let error = controller.openFailure {
                return "the list failed to open: \(error.localizedDescription)"
            }
            if let model = controller.listModel,
               controller.prototypeList != nil {
                if model.generation == generation {
                    return nil
                }
                if model.generation != 0 {
                    return "the model opened at generation "
                        + "\(model.generation), not \(generation)"
                }
            }
            if clock.now >= deadline {
                // A completion queued behind a long main-thread block
                // runs once this task yields: look once more.
                try? await Task.sleep(for: .milliseconds(500))
                if controller.listModel?.generation == generation,
                   controller.prototypeList != nil {
                    return nil
                }
                return "the list never opened at generation \(generation)"
            }
            try? await Task.sleep(for: .milliseconds(10))
        }
    }

    /// Wait, bounded at three minutes, until the main thread has
    /// answered promptly for a second: a run starts only once the last
    /// one's teardown has finished (a SwiftUI list over 500k ids takes
    /// minutes to let go), so it doesn't pay for it.
    private func settle() async {
        let started = clock.now
        let deadline = started + .seconds(180)
        var prompt = 0
        while prompt < 10, clock.now < deadline {
            let before = clock.now
            try? await Task.sleep(for: .milliseconds(100))
            prompt = clock.now - before < .milliseconds(150) ? prompt + 1 : 0
        }
        log("settled in \((clock.now - started).components.seconds) s")
    }

    /// One progress line on stderr — where a CI run spends its time,
    /// readable even if the run never finishes.
    private func log(_ message: String) {
        let elapsed = (ContinuousClock.now - startedAt).components.seconds
        FileHandle.standardError.write(
            Data("list-benchmark: \(elapsed) s: \(message)\n".utf8)
        )
    }

    /// Wait, bounded, until event `index` is render-ready — before
    /// the next input changes the state it expects (a click's
    /// selection, a position's rows).
    private func awaitRenderReady(
        _ index: Int,
        context: RunContext,
        within limit: Duration = .seconds(2)
    ) async {
        _ = await checkWithin(limit, context: context) {
            context.events[index].renderReadyTick != nil
        }
    }

    /// Wait, bounded, until every event with an expectation is
    /// render-ready, so the run's last inputs are measured before its
    /// display link stops.
    private func drainRenderReady(context: RunContext) async {
        _ = await checkWithin(.seconds(2), context: context) {
            context.events.allSatisfy {
                $0.expected == nil || $0.renderReadyTick != nil
            }
        }
    }

    // MARK: - The phases

    /// Jumps to rank ⌊f × (N − 1)⌋ for f = 0, 0.1 … 1.0, 250 ms
    /// apart, each through the seam's `scrollToRank`; after each, the
    /// populated-rows check bounded at 2 s.
    private func runJumpPhase(
        context: RunContext,
        list: any PrototypeList,
        model: ListWindowModel
    ) async -> [String] {
        var failures: [String] = []
        var previousPost = clock.now
        for i in 0 ... 10 where !context.stopped {
            let rank = Int(
                (Double(i) / 10 * Double(model.count - 1)).rounded(.down)
            )
            // Pad first, so posts land 250 ms apart even when a check
            // ran long.
            let gap = previousPost + .milliseconds(250) - clock.now
            if i > 0, gap > .zero {
                try? await Task.sleep(for: gap)
            }
            let postedAt = postInput { list.scrollToRank(rank) }
            previousPost = clock.now
            context.record(
                phase: "jumps",
                detail: "jump to rank \(rank)",
                postedAt: postedAt
            ) {
                list.visibleRanks.contains(rank)
                    && Self.allVisibleRowsPopulated(list: list, model: model)
            }
            let populated = await checkWithin(
                .seconds(2), of: previousPost, context: context
            ) {
                Self.allVisibleRowsPopulated(list: list, model: model)
            }
            if !populated {
                failures.append(
                    "jump to rank \(rank): visible rows not populated"
                        + " within 2 s"
                )
            }
        }
        return failures
    }

    /// 200 one-line scroll-wheel events upward, 16.7 ms apart,
    /// `CGEvent` → `NSEvent` → `NSApp.postEvent` — identical for both
    /// lists. Their render-ready resolves after the phase, when the
    /// per-event scroll share is known. Ends with the moved-up and
    /// populated checks.
    private func runWheelPhase(
        context: RunContext,
        window: NSWindow,
        list: any PrototypeList,
        model: ListWindowModel
    ) async -> [String] {
        var failures: [String] = []
        let windowNumber = window.windowNumber
        let bounds = list.listView.convert(list.listView.bounds, to: nil)
        let centreInWindow = NSPoint(x: bounds.midX, y: bounds.midY)
        let centre = window.convertToScreen(NSRect(
            origin: centreInWindow, size: .zero
        )).origin
        guard let baseline = Self.scrollPosition(of: list) else {
            return ["the wheel phase found no scroll position"]
        }
        // An NSEvent made from a CGEvent carries no window, so AppKit
        // can't route a posted one to this window. The harness routes
        // it as NSWindow routes scroll-wheel events: to the view under
        // the pointer, whose responder chain reaches the list's scroll
        // view — the same path for both lists.
        guard let wheelTarget = window.contentView?.superview?.hitTest(centreInWindow)
        else {
            return ["the wheel phase found no view under the list's centre"]
        }
        let start = clock.now
        for i in 0 ..< 200 where !context.stopped {
            if let event = Self.wheelEvent(
                at: centre, windowNumber: windowNumber
            ) {
                // Render-ready: the first settled tick showing the list
                // moved up from where it stood just before the event —
                // its first visible response. Line scrolls may animate,
                // so the event's full travel isn't a fixed target.
                let before = Self.scrollPosition(of: list)
                let postedAt = postInput {
                    wheelTarget.scrollWheel(with: event)
                }
                context.record(
                    phase: "wheel",
                    detail: "wheel \(i + 1)",
                    postedAt: postedAt
                ) {
                    guard let before, let now = Self.scrollPosition(of: list)
                    else {
                        return false
                    }
                    return now < before
                }
            }
            let due = start + .microseconds(16_700 * (i + 1))
            let gap = due - clock.now
            if gap > .zero {
                try? await Task.sleep(for: gap)
            }
        }

        // Let the queue drain, then the checks: the phase moved the
        // list up, and every visible row populated within 2 s.
        let populated = await checkWithin(.seconds(2), context: context) {
            Self.allVisibleRowsPopulated(list: list, model: model)
        }
        let endPosition = await settleScrollPosition(of: list)
        if !populated {
            failures.append(
                "wheel phase: visible rows not populated within 2 s"
            )
        }
        if endPosition.map({ $0 >= baseline }) ?? true {
            failures.append("the wheel phase did not move the list up")
        }
        return failures
    }

    /// Selection through posted mouse events (plan M0.10): click rank
    /// 100,000; jump to 109,999 and shift-click it; jump back to
    /// 100,000 and ⌘-click ranks 100,000…100,009. The shift-click and
    /// ⌘-click selections are checks; every step's expected state is
    /// an event for latency.
    private func runSelectionPhase(
        context: RunContext,
        window: NSWindow,
        list: any PrototypeList,
        model: ListWindowModel
    ) async -> [String] {
        var failures: [String] = []

        /// Post a click at `rank`'s row centre after positioning it
        /// on screen, then wait out its expected selection.
        func click(
            _ rank: Int,
            modifiers: NSEvent.ModifierFlags,
            expect expectedSelection: Set<Int64>,
            detail: String
        ) async -> Bool {
            let positioned = await position(
                context: context, list: list, model: model, rank: rank
            )
            guard positioned else { return false }
            guard let rect = list.rowRect(forRank: rank) else {
                failures.append("no row rect for rank \(rank)")
                return false
            }
            let postedAt = postInput {
                Self.postClick(
                    at: NSPoint(x: rect.midX, y: rect.midY),
                    modifiers: modifiers,
                    windowNumber: window.windowNumber
                )
            }
            let event = context.record(
                phase: "selection",
                detail: detail,
                postedAt: postedAt
            ) {
                list.visibleRanks.contains(rank)
                    && Self.allVisibleRowsPopulated(list: list, model: model)
                    && list.selectedIDs == expectedSelection
            }
            let took = await checkWithin(.seconds(2), context: context) {
                list.selectedIDs == expectedSelection
            }
            await awaitRenderReady(event, context: context)
            return took
        }

        let anchor = 100_000
        // 1. Plain click at 100,000 — the anchor the shift-click
        // extends from.
        let clicked = await click(
            anchor,
            modifiers: [],
            expect: Self.ids(model: model, ranks: anchor ... anchor),
            detail: "click rank \(anchor)"
        )
        if !clicked {
            failures.append("click at rank \(anchor) never took")
        }

        // 2. Jump to 109,999, shift-click it: the selection must be
        // exactly ranks 100,000…109,999.
        let shiftExpected = Self.ids(model: model, ranks: 100_000 ... 109_999)
        let shiftClicked = await click(
            109_999,
            modifiers: [.shift],
            expect: shiftExpected,
            detail: "shift-click rank 109999"
        )
        if !shiftClicked {
            failures.append(
                "shift-click: selection is not ranks 100,000…109,999"
            )
        } else {
            // 3. Back at 100,000, ⌘-click 100,000…100,009 — each
            // toggles its row out of the selection; after the last,
            // exactly 100,010…109,999 remain.
            for rank in 100_000 ... 100_009 where !context.stopped {
                let expected = Self.ids(
                    model: model, ranks: (rank + 1) ... 109_999
                )
                let toggled = await click(
                    rank,
                    modifiers: [.command],
                    expect: expected,
                    detail: "⌘-click rank \(rank)"
                )
                if !toggled {
                    failures.append(
                        "⌘-click at rank \(rank): selection never "
                            + "matched its target"
                    )
                    break
                }
            }
        }
        return failures
    }

    /// Scroll a rank on screen and wait out its landing — a position
    /// step inside the selection phase, recorded for latency.
    private func position(
        context: RunContext,
        list: any PrototypeList,
        model: ListWindowModel,
        rank: Int
    ) async -> Bool {
        if list.visibleRanks.contains(rank),
           Self.allVisibleRowsPopulated(list: list, model: model) {
            return true
        }
        let postedAt = postInput { list.scrollToRank(rank) }
        let event = context.record(
            phase: "selection",
            detail: "position jump to rank \(rank)",
            postedAt: postedAt
        ) {
            list.visibleRanks.contains(rank)
                && Self.allVisibleRowsPopulated(list: list, model: model)
        }
        let positioned = await checkWithin(.seconds(2), context: context) {
            list.visibleRanks.contains(rank)
                && Self.allVisibleRowsPopulated(list: list, model: model)
        }
        await awaitRenderReady(event, context: context)
        return positioned
    }

    // MARK: - Posting input

    /// Wrap `post` in the "input" signpost interval and return its
    /// timestamp.
    private func postInput(_ post: () -> Void) -> Double {
        let span = ListPrototypeSignposts.begin("input")
        let postedAt = CACurrentMediaTime()
        post()
        ListPrototypeSignposts.end("input", span)
        return postedAt
    }

    /// One one-line wheel event: `CGEvent` over the list's centre in
    /// global coordinates with the window-number fields set, turned
    /// into an `NSEvent` for `NSApp.postEvent` (plan M0.10).
    private static func wheelEvent(
        at screenPoint: NSPoint,
        windowNumber: Int
    ) -> NSEvent? {
        // No event source: the event is never posted to the HID
        // system, only handed to this app's queue.
        guard let cgEvent = CGEvent(
                  scrollWheelEvent2Source: nil,
                  units: .line,
                  wheelCount: 1,
                  wheel1: 1,
                  wheel2: 0,
                  wheel3: 0
              )
        else {
            return nil
        }
        let screenTop = NSScreen.screens.first?.frame.maxY ?? 0
        cgEvent.location = CGPoint(x: screenPoint.x, y: screenTop - screenPoint.y)
        cgEvent.setIntegerValueField(
            .mouseEventWindowUnderMousePointer,
            value: Int64(windowNumber)
        )
        cgEvent.setIntegerValueField(
            .mouseEventWindowUnderMousePointerThatCanHandleThisEvent,
            value: Int64(windowNumber)
        )
        return NSEvent(cgEvent: cgEvent)
    }

    /// A `mouseDown` and its `mouseUp` posted back to back in one
    /// main-actor turn, so a list's mouse-tracking loop finds the
    /// `mouseUp` at once (plan M0.10).
    private static func postClick(
        at point: NSPoint,
        modifiers: NSEvent.ModifierFlags,
        windowNumber: Int
    ) {
        let now = ProcessInfo.processInfo.systemUptime
        for (type, pressure) in [
            (NSEvent.EventType.leftMouseDown, Float(1)),
            (NSEvent.EventType.leftMouseUp, Float(0)),
        ] {
            guard let event = NSEvent.mouseEvent(
                with: type,
                location: point,
                modifierFlags: modifiers,
                timestamp: now,
                windowNumber: windowNumber,
                context: nil,
                eventNumber: 0,
                clickCount: 1,
                pressure: pressure
            ) else {
                continue
            }
            NSApp.postEvent(event, atStart: false)
        }
    }

    // MARK: - Checks and reads

    /// Poll `condition` until it holds or the deadline passes; a
    /// generation change ends the wait early.
    private func check(
        _ condition: @MainActor () -> Bool,
        context: RunContext?,
        deadline: ContinuousClock.Instant
    ) async -> Bool {
        while !condition() {
            if context?.stopped == true {
                return false
            }
            if clock.now >= deadline {
                return false
            }
            try? await Task.sleep(for: .milliseconds(5))
        }
        return true
    }

    /// `check` bounded `within` of `since` (defaults to now) — the
    /// 2 s check cadence plan M0.10 uses.
    private func checkWithin(
        _ within: Duration,
        of since: ContinuousClock.Instant? = nil,
        context: RunContext? = nil,
        _ condition: @escaping @MainActor () -> Bool
    ) async -> Bool {
        await check(
            condition,
            context: context,
            deadline: (since ?? clock.now) + within
        )
    }

    /// Wait for the scroll position to hold still across two reads —
    /// the wheel phase's queue has drained.
    private func settleScrollPosition(
        of list: any PrototypeList
    ) async -> Double? {
        var last = Self.scrollPosition(of: list)
        let deadline = clock.now + .seconds(1)
        while clock.now < deadline {
            try? await Task.sleep(for: .milliseconds(50))
            let now = Self.scrollPosition(of: list)
            if let last, let now, abs(now - last) < 0.5 {
                return now
            }
            last = now
        }
        return last
    }

    /// The seam read: every visible row populated.
    static func allVisibleRowsPopulated(
        list: any PrototypeList,
        model: ListWindowModel
    ) -> Bool {
        let ranks = list.visibleRanks
        return !ranks.isEmpty && ranks.allSatisfy {
            model.row(at: $0) != nil
        }
    }

    /// Every visible row of `pages` populated (a page scrolled out of
    /// view has none to show).
    static func deliveredRowsShown(
        _ pages: [Int],
        list: any PrototypeList,
        model: ListWindowModel
    ) -> Bool {
        let visible = list.visibleRanks
        return pages.allSatisfy { page in
            let start = page * ListWindowModel.pageSize
            let lo = max(start, visible.lowerBound)
            let hi = min(start + ListWindowModel.pageSize, visible.upperBound)
            return lo >= hi || (lo ..< hi).allSatisfy { model.row(at: $0) != nil }
        }
    }

    /// The scroll position proxy both lists answer identically:
    /// rank 0's rect top in window coordinates moves point for point
    /// with the scroll offset — upward scrolls decrease it.
    static func scrollPosition(of list: any PrototypeList) -> Double? {
        list.rowRect(forRank: 0).map { Double($0.minY) }
    }

    /// The selection's expected ids straight from the id index — the
    /// selection is held by occurrence id, so no row is fetched.
    static func ids(
        model: ListWindowModel,
        ranks: ClosedRange<Int>
    ) -> Set<Int64> {
        Set(ranks.compactMap { model.occurrenceID(at: $0) })
    }

    // MARK: - The run's numbers

    /// Reduce the tick log and the events to the report's per-run
    /// numbers (plan M0.10): per phase and overall — CPU p50/p95/max
    /// over active intervals, latency p50/p95/max over events, hitch
    /// counts — plus first-rows time, peak footprint and its rise.
    private func assembleRun(
        context: RunContext,
        implementation: PrototypeImplementation,
        pass: BenchmarkRun.Pass,
        generation: UInt64,
        failures: [String],
        windowWasKey: Bool,
        appWasActive: Bool,
        baseline: UInt64,
        screen: NSScreen?
    ) -> BenchmarkRun {
        let ticks = context.sampler.ticks
        let fps = Double(screen?.maximumFramesPerSecond ?? 0)
        let nominalMs = fps > 0 ? 1_000 / fps : 0

        // Interval i runs (tick i−1, tick i]: its CPU is the sample
        // delta.
        var cpuMs: [Double] = []
        var durations: [Double] = []
        if ticks.count > 1 {
            for i in 1 ..< ticks.count {
                durations.append(
                    (ticks[i].timestamp - ticks[i - 1].timestamp) * 1_000
                )
                cpuMs.append(
                    Double(
                        ticks[i].cpuNanoseconds
                            - ticks[i - 1].cpuNanoseconds
                    ) / 1e6
                )
            }
        }
        let runEnd = ticks.last?.timestamp ?? CACurrentMediaTime()

        var reportEvents: [BenchmarkEvent] = []
        var spans: [String: [EventSpan]] = [:]
        var latencies: [String: [Double]] = [:]
        var unresolved: [String: Int] = [:]
        var firstRowsMs: Double?
        for (position, event) in context.events.enumerated() {
            let interval = intervalIndex(of: event.postedAt, in: ticks)
            let ready = event.renderReadyTick
            // An event never render-ready is censored where the next
            // event of its phase superseded it (or the run ended): its
            // latency is a lower bound, and it counts as unresolved.
            let censoredAt = context.events[(position + 1)...]
                .first { $0.phase == event.phase }?.postedAt ?? runEnd
            let readyInterval = ready ?? intervalIndex(of: censoredAt, in: ticks)
            let latency = (ready.flatMap { ticks[safe: $0]?.timestamp } ?? censoredAt)
                - event.postedAt
            reportEvents.append(BenchmarkEvent(
                phase: event.phase,
                detail: event.detail,
                postedAtSeconds: event.postedAt,
                intervalIndex: interval,
                renderReadyIntervalIndex: ready,
                latencyMilliseconds: latency * 1_000
            ))
            if event.phase == "firstRows" {
                firstRowsMs = ready.map { (ticks[$0].timestamp - event.postedAt) * 1_000 }
            } else {
                spans[event.phase, default: []].append(EventSpan(
                    eventInterval: interval,
                    renderReadyInterval: readyInterval
                ))
                latencies[event.phase, default: []].append(latency * 1_000)
                if ready == nil {
                    unresolved[event.phase, default: 0] += 1
                }
            }
        }
        // The opening counts toward the run's overall work.
        if let first = context.events.first, first.phase == "firstRows",
           let ready = first.renderReadyTick {
            spans["overall", default: []].append(EventSpan(
                eventInterval: intervalIndex(of: first.postedAt, in: ticks),
                renderReadyInterval: ready
            ))
        }

        func phase(_ name: String) -> PhaseStatistics {
            let active = BenchmarkStatistics.activeIntervals(spans[name] ?? [])
            let cpu = active.sorted().compactMap { cpuMs[safe: $0 - 1] }
            let hitches = active.reduce(0) {
                $0 + (durations[safe: $1 - 1].map {
                    BenchmarkStatistics.isHitch(
                        duration: $0, nominalInterval: nominalMs
                    )
                } == true ? 1 : 0)
            }
            return PhaseStatistics(
                cpuMilliseconds: BenchmarkStatistics.summarize(cpu),
                latencyMilliseconds: BenchmarkStatistics.summarize(
                    latencies[name] ?? []
                ),
                hitchCount: hitches,
                eventCount: spans[name]?.count ?? 0,
                unresolvedEvents: unresolved[name] ?? 0
            )
        }

        let allSpans = (spans["jumps"] ?? [])
            + (spans["wheel"] ?? [])
            + (spans["selection"] ?? [])
            + (spans["pageDeliveries"] ?? [])
            + (spans["overall"] ?? [])
        let overallActive = BenchmarkStatistics.activeIntervals(allSpans)
        let overallCpu = overallActive.sorted().compactMap { cpuMs[safe: $0 - 1] }
        let overallLatencies = (latencies["jumps"] ?? [])
            + (latencies["wheel"] ?? [])
            + (latencies["selection"] ?? [])
            + (latencies["pageDeliveries"] ?? [])
        let overallHitches = overallActive.reduce(0) {
            $0 + (durations[safe: $1 - 1].map {
                BenchmarkStatistics.isHitch(
                    duration: $0, nominalInterval: nominalMs
                )
            } == true ? 1 : 0)
        }
        // A run-wide hitch count over every interval — stalls outside
        // event spans count too.
        let runHitches = durations.reduce(0) {
            $0 + (BenchmarkStatistics.isHitch(
                duration: $1, nominalInterval: nominalMs
            ) ? 1 : 0)
        }

        let peak = ticks.map(\.footprintBytes).max() ?? baseline
        return BenchmarkRun(
            implementation: implementation.rawValue,
            pass: pass,
            ok: failures.isEmpty && !context.generationChanged,
            failures: failures,
            windowWasKey: windowWasKey,
            appWasActive: appWasActive,
            generation: generation,
            generationChanged: context.generationChanged,
            firstRowsMilliseconds: firstRowsMs,
            baselineFootprintBytes: baseline,
            peakFootprintBytes: peak,
            footprintRiseBytes: Int64(peak) - Int64(baseline),
            phases: BenchmarkPhases(
                jumps: phase("jumps"),
                wheel: phase("wheel"),
                selection: phase("selection"),
                pageDeliveries: phase("pageDeliveries"),
                overall: PhaseStatistics(
                    cpuMilliseconds: BenchmarkStatistics.summarize(overallCpu),
                    latencyMilliseconds: BenchmarkStatistics.summarize(overallLatencies),
                    hitchCount: overallHitches,
                    eventCount: reportEvents.count,
                    unresolvedEvents: unresolved.values.reduce(0, +)
                )
            ),
            intervals: IntervalSummary(
                count: durations.count,
                activeCount: overallActive.count,
                hitchCount: runHitches,
                nominalMilliseconds: nominalMs
            ),
            events: reportEvents
        )
    }

    /// The tick index ending the interval `time` fell in: the first
    /// tick at or after it. Events posted after the last tick clamp
    /// to it.
    private func intervalIndex(
        of time: Double, in ticks: [BenchmarkTick]
    ) -> Int {
        guard ticks.count > 1 else { return 0 }
        if time <= ticks[1].timestamp { return 1 }
        if time > ticks[ticks.count - 1].timestamp {
            return ticks.count - 1
        }
        var lo = 1, hi = ticks.count - 1
        while lo < hi {
            let mid = (lo + hi) / 2
            if ticks[mid].timestamp >= time {
                hi = mid
            } else {
                lo = mid + 1
            }
        }
        return lo
    }

    /// A run that can't start still produces a report — `ok: false`.
    private func failedRun(
        _ implementation: PrototypeImplementation,
        _ pass: BenchmarkRun.Pass,
        _ generation: UInt64,
        failures: [String],
        baseline: UInt64
    ) -> BenchmarkRun {
        let empty = PhaseStatistics(
            cpuMilliseconds: BenchmarkStatistics.summarize([]),
            latencyMilliseconds: BenchmarkStatistics.summarize([]),
            hitchCount: 0,
            eventCount: 0,
            unresolvedEvents: 0
        )
        return BenchmarkRun(
            implementation: implementation.rawValue,
            pass: pass,
            ok: false,
            failures: failures,
            windowWasKey: false,
            appWasActive: false,
            generation: generation,
            generationChanged: false,
            firstRowsMilliseconds: nil,
            baselineFootprintBytes: baseline,
            peakFootprintBytes: baseline,
            footprintRiseBytes: 0,
            phases: BenchmarkPhases(
                jumps: empty, wheel: empty, selection: empty,
                pageDeliveries: empty, overall: empty
            ),
            intervals: IntervalSummary(
                count: 0, activeCount: 0, hitchCount: 0,
                nominalMilliseconds: 0
            ),
            events: []
        )
    }
}

private extension Array {
    /// `self[index]`, or nil out of range — the tick arrays are
    /// indexed by interval numbers that start at 1.
    subscript(safe index: Int) -> Element? {
        indices.contains(index) ? self[index] : nil
    }
}

extension BenchmarkEnvironment {
    /// The machine description (plan M0.10): sysctls for the model,
    /// CPU brand and the virtualized flag; the OS and Xcode versions;
    /// the main screen's refresh rate; the list's fixed shape. The
    /// baseline footprint and row count are filled by the run.
    @MainActor
    static func collect() -> BenchmarkEnvironment {
        let info = Bundle.main.infoDictionary
        let fps = Double(NSScreen.main?.maximumFramesPerSecond ?? 0)
        let size = ListPrototypeWindowController.contentSize
        return BenchmarkEnvironment(
            hwModel: SystemProbes.sysctlString("hw.model"),
            cpuBrand: SystemProbes.sysctlString("machdep.cpu.brand_string"),
            virtualized: SystemProbes.sysctlInt("kern.hv_vmm_present") != 0,
            osVersion: ProcessInfo.processInfo.operatingSystemVersionString,
            xcodeVersion: info?["DTXcode"] as? String ?? "",
            xcodeBuild: info?["DTXcodeBuild"] as? String ?? "",
            refreshRateHertz: fps,
            nominalIntervalMilliseconds: fps > 0 ? 1_000 / fps : 0,
            rowCount: 0,
            rowPitchPoints: Double(MessageRowMetrics.pitch),
            windowSize:
                "\(Int(size.width))x\(Int(size.height))",
            baselineFootprintBytes: 0
        )
    }
}
