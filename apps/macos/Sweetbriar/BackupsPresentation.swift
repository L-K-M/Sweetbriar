// Text helpers for Settings → Backups (plan M1.8j6b; spec/08). Pure
// formatters over the bridge's owned types: raw Int64 millisecond dates
// and UInt64 counts go in, concise display strings come out. Nothing
// here touches the filesystem or invents a backend state the model did
// not publish.

import Foundation
import SweetbriarBridge

enum BackupsPresentation {
    /// Epoch milliseconds as a localized date and time. A value Double
    /// cannot hold exactly stays as raw milliseconds instead of becoming
    /// a rounded date.
    static func date(_ ms: Int64?, placeholder: String = "-") -> String {
        guard let ms else { return placeholder }
        guard let seconds = Double(exactly: ms).map({ $0 / 1_000 }),
            seconds >= Date.distantPast.timeIntervalSince1970,
            seconds <= Date.distantFuture.timeIntervalSince1970
        else { return "\(ms) ms" }
        return Date(timeIntervalSince1970: seconds).formatted(date: .abbreviated, time: .shortened)
    }

    static func state(_ state: BackupState) -> String {
        switch state {
        case .inProgress: return "In progress"
        case .complete: return "Complete"
        case .failed: return "Failed"
        }
    }

    /// The latest recorded check, or that none ran. An old damaged or
    /// missing verdict is a listing detail, never a restore decision.
    static func verification(_ verification: BackupVerification?) -> String {
        guard let verification else { return "Not checked" }
        let verdict: String
        switch verification.verdict {
        case .intact: verdict = "Intact"
        case .damaged: verdict = "Damaged"
        case .missing: verdict = "Missing"
        }
        return "\(verdict), checked \(date(verification.checkedAtMs))"
    }

    static func byteCount(_ totalBytes: UInt64?) -> String {
        totalBytes.map { "\($0) bytes" } ?? "-"
    }

    static func objectCount(_ objectCount: UInt64?) -> String {
        objectCount.map { "\($0) objects" } ?? "-"
    }

    static func artifactKind(_ kind: RestoreArtifactKind) -> String {
        switch kind {
        case .preserved: return "Preserved root"
        case .staged: return "Stage"
        }
    }

    static func activity(_ activity: BackupsModel.Activity) -> String {
        switch activity {
        case .backingUp: return "Backing up…"
        case .savingDestination: return "Saving destination…"
        case .restoring(let generationId): return "Restoring \(generationId)…"
        case .cancellingRestore: return "Cancelling; still awaiting the restore's outcome…"
        }
    }

    /// Each outcome says only what the model proved: a handleless replay
    /// is not a completed copy, a stopped or failed copy is no recovery
    /// point, and a cancelled wait says nothing about the engine's copy.
    static func result(_ result: BackupsModel.ActionResult) -> String {
        switch result {
        case .backup(let generationId, let end):
            switch end {
            case .complete(let after):
                if let after {
                    return "Backup \(generationId) is complete; retention warning: \(after)"
                }
                return "Backup \(generationId) is complete."
            case .stopped:
                return "Backup \(generationId) stopped; it is not a usable recovery point."
            case .failed(let detail):
                return "Backup \(generationId) failed: \(detail). It is not a usable recovery point."
            }
        case .backupReplayed(let generationId):
            return "No new copy ran; the request replayed \(generationId)."
        case .backupWaitCancelled(let generationId):
            return "Stopped waiting for \(generationId); the engine's copy may still finish."
        case .restore(let outcome):
            return restore(outcome)
        case .requestCancelled:
            return
                "The request was cancelled. A backup already started may continue; any restore stage is retained."
        case .destinationSaved:
            return "Destination saved."
        }
    }

    /// A failed receipt after a swap still means a real restore; it is
    /// not a rollback and never a reason to retry automatically.
    static func restore(_ outcome: RestoreOutcome) -> String {
        switch outcome {
        case .restored:
            return "Restored the live archive."
        case .replayed:
            return "The request replayed an earlier restore; no new swap ran."
        case .receiptUnrecorded(let cause):
            return "Restored, but the receipt was not recorded: \(error(cause)). "
                + "The same key may restore again, so nothing retries on its own."
        case .refused(let cause):
            return "Restore refused: \(error(cause)). The live archive is unchanged; "
                + "a prepared stage is kept."
        case .closed(let cause):
            return "The core closed during restore: \(error(cause)). Whether the swap "
                + "happened is unknown; quit and reopen Sweetbriar to settle it."
        }
    }

    /// The immutable confirmation text for one chosen generation.
    static func confirmation(_ generation: BackupGeneration) -> String {
        "Restore generation \(generation.generationId) from \(date(generation.startedAtMs))? "
            + "This replaces the live archive with that recovery point, keeps the replaced "
            + "root intact, pauses every account and quarantines pending work. Nothing is "
            + "sent, moved or deleted automatically."
    }

    static func error(_ error: any Error) -> String {
        error.localizedDescription
    }
}
