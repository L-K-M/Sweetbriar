import AppKit
import Darwin
import SweetbriarBridge

/// The app's opt-in latency log (spec/08 "Latency log"): whether it is
/// on, stored in user defaults like List Implementation, and the log in
/// the cache root's `diagnostics/` once the roots are known. Recording
/// while off, or before the roots are known, does nothing.
@MainActor
final class LatencyRecorder {
    static let shared = LatencyRecorder()
    /// The user-defaults key Record Interaction Latency persists under.
    static let defaultsKey = "recordInteractionLatency"

    /// The boot time the last recorded launch saw, for warmth.
    static let bootKey = "latencyLastLaunchBoot"

    /// Waits for the first frame after a result reached `view`; false
    /// when the view isn't in a window, so nothing can show it.
    typealias FrameWait = @MainActor (NSView) async -> Bool

    private let defaults: UserDefaults
    private let frameWait: FrameWait
    private(set) var log: LatencyLog?
    private var spans = LatencySpans()
    private var launchMeasured = false

    init(defaults: UserDefaults = .standard, frameWait: @escaping FrameWait = LatencyRecorder.nextFrame) {
        self.defaults = defaults
        self.frameWait = frameWait
    }

    var isEnabled: Bool {
        get { defaults.bool(forKey: Self.defaultsKey) }
        set { defaults.set(newValue, forKey: Self.defaultsKey) }
    }

    /// Point the log at `cacheRoot`'s `diagnostics/`.
    func open(cacheRoot: URL) {
        log = LatencyLog(directory: cacheRoot.appendingPathComponent("diagnostics", isDirectory: true))
    }

    /// Record one span that ended now.
    func record(_ interaction: LatencyInteraction, milliseconds: Double, warm: Bool? = nil) {
        guard isEnabled, let log else { return }
        log.record(
            LatencyEntry(
                interaction: interaction, milliseconds: milliseconds,
                endedAtMs: Int64((Date().timeIntervalSince1970 * 1000).rounded()),
                appVersion: Bundle.main.object(forInfoDictionaryKey: "CFBundleShortVersionString") as? String
                    ?? "unknown",
                warm: warm))
    }

    /// Start a span no later start replaces, such as one compose
    /// window's open; nil while recording is off.
    func begin(_ interaction: LatencyInteraction) -> LatencySpan? {
        guard isEnabled, log != nil else { return nil }
        return LatencySpan(interaction: interaction, start: .now)
    }

    /// Start the span a later start of `interaction` replaces, under the
    /// key its result will name.
    func begin(_ interaction: LatencyInteraction, replacing key: AnyHashable) {
        guard isEnabled, log != nil else {
            spans.cancel(interaction)
            return
        }
        spans.begin(interaction, key: key, at: .now)
    }

    /// The result `key` names won't be shown.
    func cancel(_ interaction: LatencyInteraction, key: AnyHashable) {
        spans.cancel(interaction, key: key)
    }

    /// Nothing pending for `interaction` will be shown.
    func cancel(_ interaction: LatencyInteraction) {
        spans.cancel(interaction)
    }

    /// The result `key` names reached `view`: the span ends at the
    /// frame that shows it.
    func end(_ interaction: LatencyInteraction, key: AnyHashable, shownIn view: NSView) {
        end(spans.take(interaction, key: key), shownIn: view)
    }

    func end(_ span: LatencySpan?, shownIn view: NSView) {
        guard let span else { return }
        let frameWait = frameWait
        Task { @MainActor [weak self] in
            guard await frameWait(view) else { return }
            self?.record(span.interaction, milliseconds: span.milliseconds(to: .now))
        }
    }

    /// The main window's list showed its first rows in `view`: the
    /// launch span, from the process's start, ends at that frame. Only
    /// the first call counts.
    func launchShown(in view: NSView) {
        guard !launchMeasured else { return }
        launchMeasured = true
        guard isEnabled, log != nil, let started = ProcessClock.startNanoseconds(),
              let boot = ProcessClock.bootTime()
        else { return }
        let warm = LaunchWarmth.isWarm(
            previousBoot: defaults.object(forKey: Self.bootKey) as? Double, currentBoot: boot)
        defaults.set(boot, forKey: Self.bootKey)
        let frameWait = frameWait
        Task { @MainActor [weak self] in
            guard await frameWait(view) else { return }
            let elapsed = ProcessClock.nowNanoseconds() - started
            self?.record(.launch, milliseconds: Double(elapsed) / 1e6, warm: warm)
        }
    }

    /// How long a shown result may wait for its frame. A display that
    /// doesn't tick by then (a hidden or minimized window) ends the span
    /// at the limit, so a stall counts as a miss rather than vanishing.
    static let frameLimit = Duration.seconds(1)

    /// The next display-link tick of `view`'s screen, or `frameLimit`.
    static func nextFrame(of view: NSView) async -> Bool {
        guard view.window != nil else { return false }
        let clock = DisplayLinkClock(view: view)
        let limit = frameLimit
        await withTaskGroup(of: Void.self) { group in
            group.addTask { await clock.nextFrame() }
            group.addTask { try? await Task.sleep(for: limit) }
            _ = await group.next()
            // Resumes a frame wait the limit beat, so the group ends.
            clock.invalidate()
            group.cancelAll()
        }
        return true
    }

    /// The report's text, or why there is none.
    func reportText() -> String {
        guard let log else { return "The latency log opens with the archive; there is nothing to report yet." }
        let state = isEnabled ? "on" : "off"
        return "Recording is \(state). Log: \(log.fileURL.path)\n\n" + log.report().text
    }

    /// Debug → Clear Latency Log. The error, if the file couldn't be
    /// emptied.
    func clear() -> (any Error)? {
        do {
            try log?.clear()
            return nil
        } catch {
            return error
        }
    }
}

/// The process's start and the boot time, for the launch span: both on
/// `mach_absolute_time`, which the kernel stamps the start with.
enum ProcessClock {
    /// When this process started, in nanoseconds of the absolute clock.
    static func startNanoseconds() -> UInt64? {
        var info = rusage_info_v4()
        let result = withUnsafeMutablePointer(to: &info) {
            $0.withMemoryRebound(to: rusage_info_t?.self, capacity: 1) {
                proc_pid_rusage(getpid(), RUSAGE_INFO_V4, $0)
            }
        }
        guard result == 0 else { return nil }
        return nanoseconds(info.ri_proc_start_abstime)
    }

    static func nowNanoseconds() -> UInt64 { nanoseconds(mach_absolute_time()) }

    /// When the Mac started, in seconds since 1970.
    static func bootTime() -> Double? {
        var boot = timeval()
        var size = MemoryLayout<timeval>.size
        guard sysctlbyname("kern.boottime", &boot, &size, nil, 0) == 0 else { return nil }
        return Double(boot.tv_sec) + Double(boot.tv_usec) / 1e6
    }

    private static func nanoseconds(_ ticks: UInt64) -> UInt64 {
        var base = mach_timebase_info_data_t()
        mach_timebase_info(&base)
        return ticks * UInt64(base.numer) / UInt64(base.denom)
    }
}
