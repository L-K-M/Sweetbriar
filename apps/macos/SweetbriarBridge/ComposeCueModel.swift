import Observation

/// The registered appearance.compose_cue setting, not an account color.
public enum ComposeCueMode: String, Sendable, CaseIterable {
    case standard, strong

    static let settingKey = "appearance.compose_cue"
    static func decode(_ settings: [Setting]) throws -> Self {
        let values = settings.filter { $0.key == settingKey }
        guard values.count == 1, let value = values.first else {
            throw CoreFailure.unavailable(reason: "The compose cue preference is missing or duplicated.")
        }
        // Setting.value is the service's scalar, not SQLite's JSON encoding.
        guard let mode = Self(rawValue: value.value) else {
            throw CoreFailure.unavailable(reason: "The compose cue preference is not Standard or Strong.")
        }
        return mode
    }
}

/// Typed domain access over the existing settings service. No transport changes.
public protocol ComposeCueSource: AnyObject, Sendable {
    func composeCue() async throws -> ComposeCueMode
    func setComposeCue(_ mode: ComposeCueMode) async throws -> Receipt
}

extension CoreBridge: ComposeCueSource {
    public func composeCue() async throws -> ComposeCueMode {
        try ComposeCueMode.decode(await settings())
    }
    public func setComposeCue(_ mode: ComposeCueMode) async throws -> Receipt {
        try await setSetting(key: ComposeCueMode.settingKey, value: mode.rawValue)
    }
}

/// One shared, receipt-aware preference owner. Native settings controls supply
/// current write admission and use their existing frozen editor tickets too.
@MainActor @Observable
public final class ComposeCueModel {
    /// Nil is unknown, never a guessed Standard value. Failures retain an
    /// explicitly stale last acknowledged/read value until a successful retry.
    public private(set) var mode: ComposeCueMode?
    public private(set) var lastError: (any Error)?
    public private(set) var isLoading = false
    public private(set) var isSaving = false
    public private(set) var isStopped = false

    private let source: any ComposeCueSource
    private let canWrite: @MainActor () -> Bool
    private var readTicket: UInt64 = 0
    @ObservationIgnored private var reading: (id: UInt64, task: Task<Void, Never>)?
    @ObservationIgnored private var writeWaiters: [CheckedContinuation<Void, Never>] = []

    public init(source: any ComposeCueSource, canWrite: @escaping @MainActor () -> Bool = { true }) {
        self.source = source
        self.canWrite = canWrite
    }
    deinit { reading?.task.cancel() }

    /// Replacement and retirement own/await cancelled queries, even when the
    /// source ignores cancellation until its actual answer becomes available.
    public func reload() async {
        guard !isStopped, !isSaving else { return }
        let previous = reading
        previous?.task.cancel()
        readTicket &+= 1
        let ticket = readTicket
        isLoading = true
        let source = source
        let task = Task { [weak self] in
            await previous?.task.value
            guard !Task.isCancelled else { return }
            do {
                let answer = try await source.composeCue()
                guard let self, !self.isStopped, self.readTicket == ticket, !Task.isCancelled else { return }
                self.mode = answer
                self.lastError = nil
            } catch {
                guard let self, !self.isStopped, self.readTicket == ticket, !Task.isCancelled else { return }
                self.lastError = error
            }
            if let self, !self.isStopped, self.readTicket == ticket { self.isLoading = false }
        }
        reading = (ticket, task)
        await task.value
        if reading?.id == ticket { reading = nil }
    }

    /// A cancelled waiter does not discard a begun command's real receipt.
    /// The source's native cancellation boundary can still refuse an unbegun
    /// command. No automatic retry, rollback or optimistic value publication.
    @discardableResult
    public func setMode(_ mode: ComposeCueMode) async -> Bool {
        guard !isStopped, !Task.isCancelled else { return false }
        guard !isSaving else {
            lastError = CoreFailure.unavailable(reason: "Wait for the current compose cue change to finish.")
            return false
        }
        guard canWrite(), !isStopped, !Task.isCancelled else {
            if !isStopped {
                lastError = CoreFailure.unavailable(
                    reason: "Archive writes are unavailable. Try again when writing is allowed.")
            }
            return false
        }
        // An older whole-settings read must not undo the acknowledged change.
        readTicket &+= 1
        reading?.task.cancel()
        isLoading = false
        isSaving = true
        lastError = nil
        defer {
            isSaving = false
            let pending = writeWaiters
            writeWaiters.removeAll()
            for waiter in pending { waiter.resume() }
        }
        do {
            _ = try await source.setComposeCue(mode)
            if !isStopped {
                self.mode = mode
                lastError = nil
            }
            return true
        } catch {
            if !isStopped { lastError = error }
            return false
        }
    }

    public func stopAndWait() async {
        isStopped = true
        readTicket &+= 1
        isLoading = false
        let previous = reading
        previous?.task.cancel()
        if isSaving { await withCheckedContinuation { writeWaiters.append($0) } }
        await previous?.task.value
        // Keep the owner visible until it drains, so concurrent retirements
        // join the same actual answer rather than returning early.
        if reading?.id == previous?.id { reading = nil }
    }
}
