// Explicit native restore outcomes (spec/07; M1.8j4). A failed receipt
// is still a restored archive, not rollback or permission to retry automatically.

import SweetbriarCore

public enum RestoreOutcome: Sendable, Equatable {
    case restored(receipt: Receipt)
    case replayed(receipt: Receipt)
    case receiptUnrecorded(cause: CoreFailure)
    case refused(cause: CoreFailure)
    /// No usable core remains. Reopen the app to settle the restore marker.
    case closed(cause: CoreFailure)

    init(_ value: SweetbriarCore.RestoreOutcome) {
        switch value {
        case .restored(let receipt): self = .restored(receipt: Receipt(receipt))
        case .replayed(let receipt): self = .replayed(receipt: Receipt(receipt))
        case .receiptUnrecorded(let cause): self = .receiptUnrecorded(cause: CoreFailure(cause))
        case .refused(let cause): self = .refused(cause: CoreFailure(cause))
        case .closed(let cause): self = .closed(cause: CoreFailure(cause))
        }
    }
}
