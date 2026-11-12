import Observation
import SweetbriarBridge

/// One model for Settings and every main-window health surface. The session's
/// CoreBridge identity is stable even when the native core installs a new store.
@MainActor
@Observable
final class SharedBackups {
    enum DiagnosticActivity: Equatable { case benchmark, syntheticFill }

    private var current: BackupsModel?
    private var sourceID: ObjectIdentifier?
    private var diagnostic: DiagnosticActivity?
    private(set) var isStopping = false
    @ObservationIgnored private var closedRetirement: Task<Void, Never>?
    private let beforeRestore: @MainActor @Sendable () -> Void
    private let didRestore: @MainActor @Sendable () async -> Void
    private let didClose: @MainActor @Sendable () async -> Void

    init(
        beforeRestore: @escaping @MainActor @Sendable () -> Void = {},
        didRestore: @escaping @MainActor @Sendable () async -> Void,
        didClose: @escaping @MainActor @Sendable () async -> Void
    ) {
        self.beforeRestore = beforeRestore
        self.didRestore = didRestore
        self.didClose = didClose
    }

    func model(source: any BackupsSource & AnyObject) -> BackupsModel? {
        guard !isStopping else { return nil }
        if let current {
            precondition(sourceID == ObjectIdentifier(source), "one shared Backups owner per core session")
            return current
        }
        sourceID = ObjectIdentifier(source)
        let model = BackupsModel(source: source, didRestore: didRestore)
        current = model
        observeClosure(model)
        model.start()
        return model
    }

    var isRestoring: Bool {
        switch current?.activity {
        case .restoring, .cancellingRestore: true
        default: false
        }
    }

    var writesUnavailable: Bool {
        if isStopping || isRestoring || current?.isClosed == true || current?.healthError != nil {
            return true
        }
        if case .readOnly = current?.health?.integrity { return true }
        return false
    }

    /// Write UI waits for known sound health; diagnostic startup can still
    /// precede the first health read through its separate admission contract.
    var canWrite: Bool { !writesUnavailable && current?.health?.integrity == .sound }

    var restoreBlockedReason: String? {
        switch diagnostic {
        case .benchmark: "Wait for the diagnostic benchmark to finish before restoring."
        case .syntheticFill: "Wait for the synthetic fill to finish before restoring."
        case nil: nil
        }
    }

    /// Diagnostic tasks have no cooperative stop contract. Refuse overlap,
    /// rather than pretending cancellation ended their future Core calls.
    func begin(_ activity: DiagnosticActivity) -> Bool {
        guard diagnostic == nil, !writesUnavailable else { return false }
        diagnostic = activity
        return true
    }

    func end(_ activity: DiagnosticActivity) {
        precondition(diagnostic == activity, "finish only the diagnostic this owner admitted")
        diagnostic = nil
    }

    @discardableResult
    func confirmRestore() -> Bool {
        guard !isStopping, restoreBlockedReason == nil, let current,
            !current.isBusy, !current.isClosed, current.confirmation != nil
        else {
            return false
        }
        beforeRestore()
        current.confirmRestore()
        return true
    }

    /// The owner calls this before session.shutdown. Check closure directly too:
    /// an Observation callback may still be queued when a cancelled command ends.
    func cancelAndWait() async {
        isStopping = true
        await current?.cancelAndWait()
        await retireClosedContent()
    }

    private func observeClosure(_ model: BackupsModel) {
        withObservationTracking {
            _ = model.isClosed
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in await self?.retireClosedContent() }
        }
    }

    private func retireClosedContent() async {
        guard current?.isClosed == true else { return }
        if let closedRetirement {
            await closedRetirement.value
            return
        }
        let retire = didClose
        let task = Task { await retire() }
        closedRetirement = task
        await task.value
    }
}
