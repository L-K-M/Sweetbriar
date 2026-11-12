import XCTest

final class BackupsFlowTests: XCTestCase {
    @MainActor
    func testBackupRestoreClearsReplacedContentAndRetainsPathsAcrossRelaunch() throws {
        continueAfterFailure = false
        let archive = try BackupsUIArchive("roundtrip")
        addTeardownBlock { await archive.dispose() }
        let original = try archive.writeMailbox(
            "RecoveryPoint", subject: "Recovery point note", body: "snapshot-only body")
        let changed = try archive.writeMailbox(
            "ChangeAfterBackup", subject: "Unbacked note", body: "change-only body")
        try archive.launch(importPath: original)
        archive.importMail(count: 1)
        archive.chooseMailbox("RecoveryPoint", count: 1)
        archive.openAppearance()
        archive.wait(archive.settings.radioButtons, "count >= 2")
        let originalColor = archive.settings.radioButtons.element(boundBy: 0).label
        let changedColor = archive.settings.radioButtons.element(boundBy: 1).label
        XCTAssertNotEqual(originalColor, changedColor)
        archive.chooseColor(originalColor)
        archive.openBackups()
        archive.backup()
        let generation = archive.chooseGeneration()
        XCTAssertFalse(generation.isEmpty)
        XCTAssertTrue(archive.settings.buttons["backupLocationCopy-\(generation)"].exists)
        XCTAssertTrue(archive.quit())

        try archive.launch(importPath: changed)
        archive.importMail(count: 1)
        archive.chooseMailbox("ChangeAfterBackup", count: 1)
        archive.search("unbacked")
        archive.wait(archive.rows, "count == 1")
        archive.openFirstRow()
        let body = archive.main.textViews["readerBody"]
        XCTAssertTrue(body.waitForExistence(timeout: BackupsUIArchive.timeout))
        archive.wait(body, "value CONTAINS %@", "change-only body")
        archive.openAppearance()
        archive.chooseColor(changedColor)
        archive.openBackups()
        XCTAssertEqual(archive.chooseGeneration(), generation)
        archive.confirmRestore(generation)
        XCTAssertTrue(archive.waitForRestoreOutcome().hasPrefix("Restored"))
        archive.openAppearance()
        archive.wait(archive.settings.radioButtons[originalColor], "selected == true")
        XCTAssertFalse(archive.settings.radioButtons[changedColor].isSelected)
        archive.closeSettings()

        archive.wait(archive.main.searchFields["search.field"], "value == %@", "")
        XCTAssertTrue(
            archive.main.staticTexts["No message selected"].waitForExistence(
                timeout: BackupsUIArchive.timeout))
        XCTAssertFalse(archive.main.outlines["sidebar"].staticTexts["ChangeAfterBackup"].exists)
        archive.chooseMailbox("RecoveryPoint", count: 1)
        archive.openFirstRow()
        let restoredBody = archive.main.textViews["readerBody"]
        XCTAssertTrue(restoredBody.waitForExistence(timeout: BackupsUIArchive.timeout))
        archive.wait(restoredBody, "value CONTAINS %@", "snapshot-only body")
        XCTAssertFalse((restoredBody.value as? String ?? "").contains("change-only body"))

        archive.openBackups()
        let preserved = archive.settings.descendants(matching: .any)
            .matching(
                NSPredicate(
                    format: "identifier BEGINSWITH %@ AND identifier CONTAINS %@", "backupArtifact-",
                    ".preserved-")
            ).firstMatch
        XCTAssertTrue(preserved.waitForExistence(timeout: BackupsUIArchive.timeout))
        let retained = archive.artifactIDs
        XCTAssertFalse(retained.isEmpty)
        archive.settings.buttons["backupRefresh"].click()
        archive.wait(archive.settings.buttons["backupNow"], "enabled == true")
        XCTAssertEqual(archive.artifactIDs, retained)
        XCTAssertTrue(archive.quit())

        try archive.launch()
        archive.openBackups()
        XCTAssertTrue(preserved.waitForExistence(timeout: BackupsUIArchive.timeout))
        XCTAssertEqual(archive.artifactIDs, retained)
        archive.backup()
        XCTAssertTrue(archive.text(of: archive.settings.staticTexts["backupResult"]).contains("is complete"))
    }
}
