// `ImportSession`: one import job as the app follows it (spec/09 "In
// the app"), free of AppKit so the bridge's Linux tests exercise it.
//
//   start(paths) ─► inventory ─► awaitingConfirmation ─► confirm ─►
//   running ─► completed | cancelled | failed ─► report written?
//                     │                              └─ pending: retry
//                     └─ cancel waits for cancel_job's receipt
//
// It polls `import_job` with at most one request in flight, chooses a
// variant per ambiguous source, and keeps the job's id so a restarted
// app reattaches. `detach` stops following without touching the job:
// closing the progress sheet leaves the job running.

import Observation

/// The import queries and commands a session needs from the core
/// (spec/07). `CoreBridge` adopts it; tests substitute a fake.
public protocol ImportJobSource: Sendable {
    func startImport(paths: [String], target: ImportTarget) async throws -> ImportStarted
    func confirmImport(jobId: String, variants: [VariantChoice]) async throws -> Receipt
    func cancelJob(jobId: String) async throws -> Receipt
    func importJob(id: String) async throws -> ImportJob
    func importReport(jobId: String) async throws -> ImportReport?
}

extension CoreBridge: ImportJobSource {}

@MainActor
@Observable
public final class ImportSession {
    /// How often the job is polled while it isn't finished, and while
    /// a finished job's report is still being written.
    public static let defaultPollInterval: Duration = .milliseconds(250)
    /// How long a finished job's report is looked for before it shows
    /// as pending, with Retry: it is normally written at once.
    public static let defaultReportPatience: Duration = .seconds(10)

    /// The finished job's report, as far as the app knows it.
    public enum ReportState: Equatable, Sendable {
        /// Not looked for yet: the job hasn't finished.
        case notFinished
        /// The job finished but the report isn't written yet (spec/09:
        /// never shown as written).
        case pending
        case written(ImportReport)
    }

    /// The job's id once started or attached.
    public private(set) var jobId: String?
    /// The job's last state as polled.
    public private(set) var job: ImportJob?
    /// The variant chosen per ambiguous source, by ordinal.
    public private(set) var variants: [UInt64: MboxVariant] = [:]
    public private(set) var report: ReportState = .notFinished
    /// The last failure of a command or a poll; polling goes on.
    public private(set) var lastError: (any Error)?
    /// True while `cancel` waits for `cancel_job`'s receipt.
    public private(set) var cancelling = false

    /// Called on the main actor after each poll that changed the job,
    /// so the app can refresh what the import changed (the sidebar).
    @ObservationIgnored
    public var onJobChanged: @MainActor (ImportJob) -> Void = { _ in }

    private let source: any ImportJobSource
    private let pollInterval: Duration
    private let reportPatience: Duration
    @ObservationIgnored
    private var polling: Task<Void, Never>?

    public init(
        source: any ImportJobSource,
        pollInterval: Duration = ImportSession.defaultPollInterval,
        reportPatience: Duration = ImportSession.defaultReportPatience
    ) {
        self.source = source
        self.pollInterval = pollInterval
        self.reportPatience = reportPatience
    }

    /// The sources that need a variant before `confirm` (spec/09).
    public var ambiguousSources: [ImportSource] {
        job?.sources.filter(\.needsVariant) ?? []
    }

    /// Whether `confirm` can run: the job waits for confirmation and
    /// every ambiguous source has its variant.
    public var canConfirm: Bool {
        guard job?.state == .awaitingConfirmation, !cancelling else { return false }
        return ambiguousSources.allSatisfy { variants[$0.ordinal] != nil }
    }

    /// Start a job over `paths` and follow it.
    public func start(paths: [String]) async throws {
        let started = try await source.startImport(paths: paths, target: .localArchive)
        attach(jobId: started.jobId)
    }

    /// Follow an existing job — the one a restarted app kept.
    public func attach(jobId id: String) {
        polling?.cancel()
        jobId = id
        job = nil
        variants = [:]
        report = .notFinished
        lastError = nil
        startPolling()
    }

    /// Stop following; the job itself runs on (spec/09). The loop's
    /// request in flight, if any, still ends before another starts.
    public func detach() {
        polling?.cancel()
    }

    /// Choose `variant` for the ambiguous source at `ordinal`.
    public func choose(_ variant: MboxVariant, forSource ordinal: UInt64) {
        variants[ordinal] = variant
    }

    /// Take back the choice for the source at `ordinal`: it needs one
    /// again before `confirm`.
    public func clearChoice(forSource ordinal: UInt64) {
        variants[ordinal] = nil
    }

    /// Confirm the inventory with the chosen variants, then follow the
    /// ingest.
    public func confirm() async throws {
        guard let jobId, canConfirm else { return }
        let choices = ambiguousSources.compactMap { source in
            variants[source.ordinal].map { VariantChoice(ordinal: source.ordinal, variant: $0) }
        }
        _ = try await source.confirmImport(jobId: jobId, variants: choices)
        startPolling()
    }

    /// Cancel the job and wait for `cancel_job`'s receipt (spec/09);
    /// committed messages stay imported.
    public func cancel() async throws {
        guard let jobId, !(job?.state.isFinished ?? false) else { return }
        cancelling = true
        defer { cancelling = false }
        _ = try await source.cancelJob(jobId: jobId)
        startPolling()
    }

    /// Look for a pending report again.
    public func retryReport() {
        guard report == .pending else { return }
        startPolling()
    }

    /// One poll loop at a time: each poll waits for its answer before
    /// the next starts, so at most one request is in flight. It ends
    /// once the job waits for confirmation, once the report is
    /// written, or `reportPatience` after the job was seen finished.
    private func startPolling() {
        let previous = polling
        previous?.cancel()
        guard let jobId else { return }
        let source = self.source
        let interval = pollInterval
        let patience = reportPatience
        polling = Task { [weak self] in
            // The previous loop's request ends first: one in flight.
            await previous?.value
            var giveUp: ContinuousClock.Instant?
            while !Task.isCancelled {
                do {
                    let job = try await source.importJob(id: jobId)
                    guard let self, !Task.isCancelled else { return }
                    let changed = job != self.job
                    self.job = job
                    self.lastError = nil
                    if changed {
                        self.onJobChanged(job)
                    }
                    if job.state == .awaitingConfirmation {
                        // Nothing changes until `confirm` or `cancel`.
                        return
                    }
                    if job.state.isFinished {
                        let deadline = giveUp ?? .now + patience
                        giveUp = deadline
                        if let found = try await source.importReport(jobId: jobId) {
                            self.report = .written(found)
                            return
                        }
                        self.report = .pending
                        if ContinuousClock.now >= deadline {
                            return
                        }
                    }
                } catch is CancellationError {
                    return
                } catch {
                    guard let self else { return }
                    self.lastError = error
                    // A finished job whose report can't be read waits
                    // for Retry once the patience runs out, like one
                    // not written yet.
                    if let deadline = giveUp {
                        self.report = .pending
                        if ContinuousClock.now >= deadline {
                            return
                        }
                    }
                }
                try? await Task.sleep(for: interval)
            }
        }
    }
}
