import Foundation
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@Suite
@MainActor
struct BackupsPresentationTests {
    @Test func countsAndDatesKeepTheirFullDomain() {
        #expect(BackupsPresentation.byteCount(UInt64.max) == "18446744073709551615 bytes")
        #expect(BackupsPresentation.objectCount(UInt64.max) == "18446744073709551615 objects")
        #expect(BackupsPresentation.byteCount(nil) == "-")
        #expect(BackupsPresentation.date(nil) == "-")
        #expect(BackupsPresentation.date(Int64.min) == "-9223372036854775808 ms")
        #expect(BackupsPresentation.date(Int64.max) == "9223372036854775807 ms")
        #expect(!BackupsPresentation.date(1_800_000_000_000).isEmpty)
        #expect(BackupsPresentation.verification(nil) == "Not checked")
        for verdict in [BackupVerdict.intact, .damaged, .missing] {
            #expect(
                BackupsPresentation.verification(.init(verdict: verdict, checkedAtMs: 0)).contains("checked"))
        }
    }

    @Test func everyResultPreservesItsMeaning() {
        let receipt = Receipt(command: "restore", resultJson: "{}", createdAtMs: 0)
        let cause = CoreFailure.unavailable(reason: "synthetic failure")
        let cases: [(BackupsModel.ActionResult, String)] = [
            (.backup(generationId: "G", end: .complete(after: nil)), "is complete"),
            (.backup(generationId: "G", end: .complete(after: "retention")), "complete; retention warning"),
            (.backup(generationId: "G", end: .stopped), "not a usable recovery point"),
            (.backup(generationId: "G", end: .failed(detail: "broken")), "not a usable recovery point"),
            (.backupReplayed(generationId: "G"), "No new copy"),
            (.backupWaitCancelled(generationId: "G"), "engine's copy may still finish"),
            (.requestCancelled, "any restore stage is retained"),
            (.destinationSaved, "Destination saved"),
            (.restore(.restored(receipt: receipt)), "Restored the live archive"),
            (.restore(.replayed(receipt: receipt)), "no new swap"),
            (.restore(.receiptUnrecorded(cause: cause)), "Restored, but the receipt was not recorded"),
            (.restore(.refused(cause: cause)), "live archive is unchanged"),
            (.restore(.closed(cause: cause)), "Whether the swap happened is unknown"),
        ]
        for (result, expected) in cases {
            #expect(BackupsPresentation.result(result).contains(expected))
        }
        #expect(
            BackupsPresentation.restore(.receiptUnrecorded(cause: cause)).contains(
                "same key may restore again"))
        #expect(BackupsPresentation.restore(.refused(cause: cause)).contains("stage is kept"))
        #expect(BackupsPresentation.restore(.closed(cause: cause)).contains("quit and reopen"))
    }

    @Test func confirmationNamesTheImmutableGenerationAndConsequences() {
        let row = BackupGeneration(
            generationId: "chosen-immutable-id", state: .complete, startedAtMs: 0,
            completedAtMs: 1, location: "/synthetic/recovery", objectCount: 2,
            totalBytes: 3, verification: .init(verdict: .damaged, checkedAtMs: 4))
        let text = BackupsPresentation.confirmation(row)
        #expect(text.contains(row.id) && text.contains(BackupsPresentation.date(row.startedAtMs)))
        for consequence in [
            "replaces the live archive", "keeps the replaced root intact", "pauses every account",
            "quarantines pending work", "Nothing is sent, moved or deleted automatically",
        ] {
            #expect(text.contains(consequence))
        }
    }
}
