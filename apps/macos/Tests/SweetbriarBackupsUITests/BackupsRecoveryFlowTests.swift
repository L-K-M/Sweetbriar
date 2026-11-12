import XCTest

final class BackupsRecoveryFlowTests: XCTestCase {
    @MainActor
    func testUnavailableDestinationNeverCreatesItOrFallsBack() throws {
        continueAfterFailure = false
        let archive = try BackupsUIArchive("destination")
        addTeardownBlock { await archive.dispose() }
        try archive.launch()
        archive.openBackups()
        let missing = archive.root.appendingPathComponent("unmounted/Backups")
        let field = archive.settings.textFields["backupDestination"]
        archive.wait(field, "enabled == true")
        field.click()
        field.typeKey("a", modifierFlags: .command)
        field.typeKey(.delete, modifierFlags: [])
        // Observe the editor, not just synthetic key delivery, before saving.
        archive.wait(field, "value == %@", "")
        field.typeText(missing.path)
        archive.wait(field, "value == %@", missing.path)
        field.typeKey(.tab, modifierFlags: [])
        archive.wait(field, "value == %@", missing.path)
        archive.settings.buttons["backupApply"].click()
        archive.wait(archive.settings.staticTexts["backupResult"], "value == %@", "Destination saved.")
        XCTAssertTrue(archive.quit())

        try archive.launch()
        archive.openBackups()
        archive.wait(archive.settings.textFields["backupDestination"], "value == %@", missing.path)
        archive.settings.buttons["backupNow"].click()
        let error = archive.settings.staticTexts["backupCommandError"]
        XCTAssertTrue(error.waitForExistence(timeout: BackupsUIArchive.timeout))
        XCTAssertFalse(archive.text(of: error).isEmpty)
        archive.wait(archive.settings.buttons["backupNow"], "enabled == true")
        XCTAssertFalse(FileManager.default.fileExists(atPath: missing.deletingLastPathComponent().path))
        XCTAssertFalse(archive.settings.staticTexts["Complete"].exists)
        archive.settings.buttons["backupDefault"].click()
        archive.wait(archive.settings.staticTexts["backupResult"], "value == %@", "Destination saved.")
        archive.backup()
        XCTAssertFalse(FileManager.default.fileExists(atPath: missing.path))
    }

    @MainActor
    func testReadOnlyBannerAllowsFreshVerifiedRestore() throws {
        continueAfterFailure = false
        let archive = try BackupsUIArchive("readonly")
        addTeardownBlock { await archive.dispose() }
        let mailbox = try archive.writeMailbox(
            "ReadOnlyRecovery", subject: "Recovery note", body: "read-only recovery body")
        try archive.launch(importPath: mailbox)
        archive.importMail(count: 1)
        archive.openBackups()
        archive.backup()
        XCTAssertTrue(archive.quit())
        try Data("not json".utf8).write(to: archive.data.appendingPathComponent("integrity-latch.json"))
        try archive.launch()
        let bannerButton = archive.main.buttons["openBackups"]
        XCTAssertTrue(bannerButton.waitForExistence(timeout: BackupsUIArchive.timeout))
        archive.app.menuBars.menuBarItems["File"].click()
        XCTAssertFalse(archive.app.menuBars.menuItems["Import…"].isEnabled)
        archive.app.typeKey(.escape, modifierFlags: [])
        bannerButton.click()
        XCTAssertTrue(
            archive.settings.buttons["backupNow"].waitForExistence(timeout: BackupsUIArchive.timeout))
        XCTAssertFalse(archive.settings.buttons["backupNow"].isEnabled)
        XCTAssertFalse(archive.settings.textFields["backupDestination"].isEnabled)
        let generation = archive.chooseGeneration()
        archive.confirmRestore(generation)
        XCTAssertTrue(archive.waitForRestoreOutcome().hasPrefix("Restored"))
        archive.closeSettings()
        archive.wait(archive.main.buttons["openBackups"], "exists == false")
        archive.chooseMailbox("ReadOnlyRecovery", count: 1)
        archive.rows.element(boundBy: 0).click()
        archive.wait(archive.main.textViews["readerBody"], "value CONTAINS %@", "read-only recovery body")
    }

    @MainActor
    func testMissingGenerationRefusesWithoutLosingTheLiveCore() throws {
        continueAfterFailure = false
        let archive = try BackupsUIArchive("missing-generation")
        addTeardownBlock { await archive.dispose() }
        let mailbox = try archive.writeMailbox(
            "StillLive", subject: "Still live", body: "unchanged live body")
        try archive.launch(importPath: mailbox)
        archive.importMail(count: 1)
        archive.openBackups()
        archive.backup()
        let generation = archive.chooseGeneration()
        let shown = archive.settings.descendants(matching: .any)["backupLocation-\(generation)"]
        XCTAssertTrue(shown.exists)
        let location = URL(fileURLWithPath: archive.text(of: shown)).resolvingSymlinksInPath()
        guard location.path.hasPrefix(archive.root.path + "/") else {
            XCTFail("Only a fixture-owned generation may be moved")
            return
        }
        let parked = archive.root.appendingPathComponent("unavailable-generation")
        try FileManager.default.moveItem(at: location, to: parked)
        archive.confirmRestore(generation)
        XCTAssertTrue(
            archive.settings.staticTexts["backupCommandError"].waitForExistence(
                timeout: BackupsUIArchive.timeout))
        archive.wait(archive.settings.buttons["backupNow"], "enabled == true")
        archive.closeSettings()
        archive.chooseMailbox("StillLive", count: 1)
        archive.rows.element(boundBy: 0).click()
        archive.wait(archive.main.textViews["readerBody"], "value CONTAINS %@", "unchanged live body")
        // Deliberate fixture repair, followed by a new explicit user command.
        try FileManager.default.moveItem(at: parked, to: location)
        archive.openBackups()
        XCTAssertEqual(archive.chooseGeneration(), generation)
        archive.confirmRestore(generation)
        XCTAssertTrue(archive.waitForRestoreOutcome().hasPrefix("Restored"))
    }

    @MainActor
    func testCancelAwaitsTheActualRestoreAndRetainsEvidence() throws {
        continueAfterFailure = false
        let archive = try BackupsUIArchive("cancel")
        addTeardownBlock { await archive.dispose() }
        // Enough distinct objects for UI automation to observe real preparation,
        // not a fake delay or an unbegun call. Import/index has its own UI gate.
        let baseline = try archive.writeMailbox(
            "CancelBaseline", subject: "Baseline", body: "baseline body", count: 5001)
        let change = try archive.writeMailbox("CancelChange", subject: "Later", body: "unbacked change")
        try archive.launch(importPath: baseline)
        // Seeding is not a performance assertion: observed 1,750/5,001
        // messages after one minute on this filesystem. Keep small flows at 60s.
        archive.importMail(count: 5001, completionTimeout: 300)
        archive.openBackups()
        archive.backup(completionTimeout: 300)
        XCTAssertTrue(archive.quit())
        try archive.launch(importPath: change)
        archive.importMail(count: 1)
        archive.openBackups()
        let generation = archive.chooseGeneration()
        archive.confirmRestore(generation)
        let root = archive.root
        let staging = XCTNSPredicateExpectation(
            predicate: NSPredicate { _, _ in
                let names = (try? FileManager.default.contentsOfDirectory(atPath: root.path)) ?? []
                return names.contains { $0.hasPrefix("data.restoring-") }
            }, object: nil)
        XCTAssertEqual(XCTWaiter.wait(for: [staging], timeout: BackupsUIArchive.timeout), .completed)
        let cancel = archive.settings.buttons["backupCancelRestore"]
        XCTAssertTrue(
            cancel.exists, "Cancellation must actually be requested, not inferred from a fast success")
        cancel.click()
        let restored: Bool
        switch archive.waitForRestoreCompletion() {
        case .result(let outcome):
            restored = outcome.hasPrefix("Restored")
            XCTAssertTrue(
                restored || outcome.hasPrefix("The request was cancelled.")
                    || outcome.hasPrefix("Restore refused:"))
        case .error(let detail):
            // Cancelling a begun manifest read can return its real Interrupted
            // I/O error, not the facade's pre-admission CancellationError.
            XCTAssertTrue(detail.contains("operation interrupted"), detail)
            restored = false
        }
        let retained = try FileManager.default.contentsOfDirectory(atPath: root.path).filter {
            $0.hasPrefix(restored ? "data.preserved-" : "data.restoring-")
        }
        XCTAssertFalse(retained.isEmpty)
        archive.settings.buttons["backupRefresh"].click()
        for name in retained {
            let shown = archive.settings.descendants(matching: .any).matching(
                NSPredicate(
                    format: "identifier BEGINSWITH %@ AND identifier ENDSWITH %@", "backupArtifact-",
                    "/" + name
                )
            ).firstMatch
            XCTAssertTrue(shown.waitForExistence(timeout: BackupsUIArchive.timeout))
        }
        archive.closeSettings()
        let changedMailbox = archive.main.outlines["sidebar"].staticTexts["CancelChange"]
        if restored {
            XCTAssertFalse(changedMailbox.exists)
        } else {
            archive.chooseMailbox("CancelChange", count: 1)
        }
        XCTAssertTrue(archive.main.outlines["sidebar"].staticTexts["CancelBaseline"].exists)
    }

    @MainActor
    func testRestoreClosesAnOpenImportPicker() throws {
        continueAfterFailure = false
        let archive = try BackupsUIArchive("open-picker")
        addTeardownBlock { await archive.dispose() }
        try archive.launch()
        archive.openBackups()
        archive.backup()
        let generation = archive.chooseGeneration()
        archive.app.menuBars.menuBarItems["File"].click()
        archive.app.menuBars.menuItems["Import…"].click()
        XCTAssertTrue(archive.main.sheets.firstMatch.waitForExistence(timeout: BackupsUIArchive.timeout))
        archive.openBackups()
        archive.confirmRestore(generation)
        XCTAssertTrue(archive.waitForRestoreOutcome().hasPrefix("Restored"))
        XCTAssertFalse(archive.main.sheets.firstMatch.exists)
    }
}
