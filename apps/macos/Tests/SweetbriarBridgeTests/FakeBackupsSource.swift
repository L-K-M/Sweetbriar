import Foundation

@testable import SweetbriarBridge

final class FakeBackupsHealthEvents: BackupsHealthEvents {
    private let stream: AsyncThrowingStream<Void, any Error>
    private let continuation: AsyncThrowingStream<Void, any Error>.Continuation

    init() {
        (stream, continuation) = AsyncThrowingStream.makeStream()
    }

    func next() async throws {
        var iterator = stream.makeAsyncIterator()
        if try await iterator.next() == nil {
            try Task.checkCancellation()
            throw CoreFailure.unavailable(reason: "fake stream closed")
        }
    }

    func emit() { continuation.yield(()) }
    func reset() { continuation.finish(throwing: CoreFailure.resetRequired(generation: 0)) }
    func fail(_ error: CoreFailure) { continuation.finish(throwing: error) }
}

struct FakeBackupsCopy: BackupCopy {
    let end: BackupEnd
    var gate: Gate?

    func finished() async throws -> BackupEnd {
        if let gate { await gate.wait() }
        try Task.checkCancellation()
        return end
    }
}

func backupRow(
    _ id: String = "generation-1", state: BackupState = .complete,
    date: Int64 = 1_000, verdict: BackupVerdict? = nil
) -> BackupGeneration {
    BackupGeneration(
        generationId: id, state: state, startedAtMs: date,
        completedAtMs: state == .complete ? date + 1 : nil,
        location: "/synthetic/backups/\(id)", objectCount: 3, totalBytes: 41,
        verification: verdict.map { BackupVerification(verdict: $0, checkedAtMs: 2_000) }
    )
}

func backupsReceipt(_ command: String = "start_restore") -> Receipt {
    Receipt(command: command, resultJson: "{}", createdAtMs: 1_001)
}

actor FakeBackupsSource: BackupsSource {
    enum Call: Equatable {
        case rows, artifacts, settings, subscribe, health
        case backup(String)
        case restore(String, String)
        case setting(String, String)
    }

    private(set) var calls: [Call] = []
    private(set) var cancelledReads: [Bool] = []
    private(set) var restoreWasCancelled = false
    private(set) var feeds: [FakeBackupsHealthEvents] = []
    private var rows = [backupRow()]
    private var names = [RestoreArtifact(path: "/synthetic/data.restoring-name", kind: .staged)]
    private var destination = ""
    private var missingDestination = false
    private var currentHealth = Health(integrity: .sound)
    private var rowGate: Gate?
    private var rowFailure: CoreFailure?
    private var healthGate: Gate?
    private var restoreGate: Gate?
    private var nextFailure: CoreFailure?
    private var commandCancellation = false
    private var readFailure: CoreFailure?
    private var healthFailure: CoreFailure?
    private var settingsFailure: CoreFailure?
    private var copy: (any BackupCopy)?
    private var restoreAnswer = RestoreOutcome.restored(receipt: backupsReceipt())

    func setRows(_ rows: [BackupGeneration]) { self.rows = rows }
    func setDestination(_ value: String) { destination = value }
    func omitDestination() { missingDestination = true }
    func setHealth(_ health: Health) { currentHealth = health }
    func holdRows(_ gate: Gate?) { rowGate = gate }
    func failRows(_ error: CoreFailure?) { rowFailure = error }
    func holdHealth(_ gate: Gate?) { healthGate = gate }
    func holdRestore(_ gate: Gate?) { restoreGate = gate }
    func failNextCommand(_ error: CoreFailure) { nextFailure = error }
    func cancelNextCommand() { commandCancellation = true }
    func failReads(_ error: CoreFailure?) { readFailure = error }
    func failHealth(_ error: CoreFailure?) { healthFailure = error }
    func failSettings(_ error: CoreFailure?) { settingsFailure = error }
    func answerBackup(_ copy: (any BackupCopy)?) { self.copy = copy }
    func answerRestore(_ value: RestoreOutcome) { restoreAnswer = value }
    var rowReads: Int { calls.filter { $0 == .rows }.count }
    var healthReads: Int { calls.filter { $0 == .health }.count }
    var subscriptionCount: Int { feeds.count }

    func listBackups() async throws -> [BackupGeneration] {
        calls.append(.rows)
        cancelledReads.append(Task.isCancelled)
        if let rowFailure { throw rowFailure }
        let value = rows
        if let rowGate { await rowGate.wait() }
        return value
    }

    func listRestoreArtifacts() async throws -> [RestoreArtifact] {
        calls.append(.artifacts)
        if let readFailure { throw readFailure }
        return names
    }

    func settings() async throws -> [Setting] {
        calls.append(.settings)
        if let settingsFailure { throw settingsFailure }
        return missingDestination
            ? [] : [Setting(key: "backups.destination", value: destination, isDefault: destination.isEmpty)]
    }

    func health() async throws -> Health {
        calls.append(.health)
        if let healthFailure { throw healthFailure }
        let value = currentHealth
        if let healthGate { await healthGate.wait() }
        return value
    }

    func subscribeHealth() async throws -> FakeBackupsHealthEvents {
        calls.append(.subscribe)
        let feed = FakeBackupsHealthEvents()
        feeds.append(feed)
        return feed
    }

    func setSetting(key: String, value: String) async throws -> Receipt {
        calls.append(.setting(key, value))
        try failCommand()
        destination = value
        return backupsReceipt("set_setting")
    }

    func startBackup(idempotencyKey: String) async throws -> BackupStarted {
        calls.append(.backup(idempotencyKey))
        try failCommand()
        return BackupStarted(
            receipt: backupsReceipt("start_backup"), generationId: "generation-1", copy: copy)
    }

    func startRestore(idempotencyKey: String, generationId: String) async throws -> RestoreOutcome {
        calls.append(.restore(idempotencyKey, generationId))
        if let restoreGate { await restoreGate.wait() }
        restoreWasCancelled = Task.isCancelled
        try failCommand()
        switch restoreAnswer {
        case .restored, .receiptUnrecorded:
            rows = [backupRow("restored-generation", date: 10)]
            destination = "/synthetic/restored-destination"
            currentHealth = Health(integrity: .sound)
        default: break
        }
        return restoreAnswer
    }

    private func failCommand() throws {
        if commandCancellation {
            commandCancellation = false
            throw CancellationError()
        }
        if let nextFailure {
            self.nextFailure = nil
            throw nextFailure
        }
    }
}
