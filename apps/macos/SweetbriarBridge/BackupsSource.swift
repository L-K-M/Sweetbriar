/// The Backups model's owned bridge surface. Tests supply a scripted source;
/// the app uses its existing CoreBridge, never a second core.
public protocol BackupsSource: Sendable {
    associatedtype HealthEvents: BackupsHealthEvents

    func listBackups() async throws -> [BackupGeneration]
    func listRestoreArtifacts() async throws -> [RestoreArtifact]
    func settings() async throws -> [Setting]
    func health() async throws -> Health
    func subscribeHealth() async throws -> HealthEvents
    func setSetting(key: String, value: String) async throws -> Receipt
    func startBackup(idempotencyKey: String) async throws -> BackupStarted
    func startRestore(idempotencyKey: String, generationId: String) async throws -> RestoreOutcome
}

/// One in-flight next per stream; every answer requires a new health read.
public protocol BackupsHealthEvents: Sendable {
    func next() async throws
}

extension CoreBridge: BackupsSource {}
extension HealthSubscription: BackupsHealthEvents {}
