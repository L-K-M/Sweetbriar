import SweetbriarCore

/// A wait on the engine's running copy, not ownership of that work.
/// Cancellation stops only the wait; another wait still gets the answer.
public protocol BackupCopy: Sendable {
    func finished() async throws -> BackupEnd
}

public struct BackupStarted: Sendable {
    public var receipt: Receipt
    public var generationId: String
    /// Nil on replay: nothing was started by this call.
    public var copy: (any BackupCopy)?

    public init(receipt: Receipt, generationId: String, copy: (any BackupCopy)?) {
        self.receipt = receipt
        self.generationId = generationId
        self.copy = copy
    }

    init(_ value: SweetbriarCore.BackupStarted) {
        receipt = Receipt(value.receipt)
        generationId = value.generationId
        copy = value.copy.map { NativeBackupCopy(inner: $0) as any BackupCopy }
    }
}

public enum BackupEnd: Sendable, Equatable {
    /// The generation stands, including when retention reported a warning.
    case complete(after: String?)
    case stopped
    case failed(detail: String)

    init(_ value: SweetbriarCore.BackupEnd) {
        switch value {
        case .complete(let after): self = .complete(after: after)
        case .stopped: self = .stopped
        case .failed(let detail): self = .failed(detail: detail)
        }
    }
}

private struct NativeBackupCopy: BackupCopy {
    let inner: SweetbriarCore.BackupCopy

    func finished() async throws -> BackupEnd {
        let token = SweetbriarCore.CancelToken()
        let value = try await CoreBridge.withCancellation(token: token) {
            try await inner.finished(token: token)
        }
        return BackupEnd(value)
    }
}
