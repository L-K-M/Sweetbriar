import XCTest
import SQLite3

/// The M2.7e1 account settings flow (spec/08 "Settings"; spec/07
/// "Account management"): seed one synthetic Proton account into a
/// fresh store, rename it and pause it through Settings → Accounts,
/// then read the stored name and state back. Synthetic content only.
final class AccountSettingsFlowTests: XCTestCase {
    private static let timeout: TimeInterval = 60
    private static let accountID = "22222222-2222-2222-2222-222222222222"
    private static let seededName = "Owner"
    private static let renamedName = "Renamed Proton"

    @MainActor
    func testRenameAndPauseLeaveTheirStoredStates() throws {
        continueAfterFailure = false
        let roots = FileManager.default.temporaryDirectory
            .appendingPathComponent("sweetbriar-account-settings-\(UUID().uuidString)")
        let dataRoot = roots.appendingPathComponent("data")
        let cacheRoot = roots.appendingPathComponent("cache")
        defer { try? FileManager.default.removeItem(at: roots) }

        // The first launch creates the store; the seed needs the
        // schema and must not run while a core holds the lock.
        let opener = XCUIApplication()
        opener.launchArguments = Self.launchArguments(dataRoot: dataRoot, cacheRoot: cacheRoot)
        opener.launch()
        let openerWindow = opener.windows["Sweetbriar"]
        XCTAssertTrue(openerWindow.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(openerWindow.outlines["sidebar"].staticTexts["Local Archive"]
            .waitForExistence(timeout: Self.timeout))
        opener.terminate()
        XCTAssertTrue(opener.wait(for: .notRunning, timeout: Self.timeout))

        let store = dataRoot.appendingPathComponent("state.sqlite").path
        try Self.seedProtonAccount(store)

        let app = XCUIApplication()
        app.launchArguments = Self.launchArguments(dataRoot: dataRoot, cacheRoot: cacheRoot)
        app.launch()
        let window = app.windows["Sweetbriar"]
        XCTAssertTrue(window.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(window.outlines["sidebar"].staticTexts["Local Archive"]
            .waitForExistence(timeout: Self.timeout))

        app.menuBars.menuBarItems["Sweetbriar"].click()
        app.menuBars.menuItems["Settings…"].click()

        // The tab picker's items are tabs on macOS 27 and other button
        // kinds on macOS 26, so the query takes any of them by label.
        let settings = app.windows["Settings"]
        XCTAssertTrue(settings.waitForExistence(timeout: Self.timeout))
        let tabKinds: [XCUIElement.ElementType] = [.tab, .radioButton, .button]
        let accountsTab = settings.descendants(matching: .any)
            .matching(NSPredicate(
                format: "label == %@ AND elementType IN %@", "Accounts",
                tabKinds.map { $0.rawValue }))
            .firstMatch
        XCTAssertTrue(accountsTab.waitForExistence(timeout: Self.timeout))
        accountsTab.click()

        let nameField = settings.textFields["account-name-\(Self.accountID)"]
        XCTAssertTrue(nameField.waitForExistence(timeout: Self.timeout))
        XCTAssertEqual(nameField.value as? String, Self.seededName)
        nameField.click()
        nameField.typeKey("a", modifierFlags: .command)
        nameField.typeKey(.delete, modifierFlags: [])
        nameField.typeText(Self.renamedName)
        settings.buttons["account-rename-\(Self.accountID)"].click()

        // The rename's status lands only after its re-read, so the busy
        // window is closed when it appears and the pause click is taken.
        let status = settings.staticTexts["accountCommandStatus"]
        XCTAssertTrue(status.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(waitForValue(status, "Renamed"))

        let pause = settings.buttons["account-pause-\(Self.accountID)"]
        XCTAssertTrue(pause.waitForExistence(timeout: Self.timeout))
        pause.click()

        let state = settings.staticTexts["account-state-\(Self.accountID)"]
        XCTAssertTrue(state.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(waitForValue(state, "Paused"))

        app.terminate()
        XCTAssertTrue(app.wait(for: .notRunning, timeout: Self.timeout))

        let stored = try Self.accountRow(store, id: Self.accountID)
        XCTAssertEqual(stored?.displayName, Self.renamedName)
        XCTAssertEqual(stored?.state, "paused")
    }

    private static func launchArguments(dataRoot: URL, cacheRoot: URL) -> [String] {
        [
            "-dataRoot", dataRoot.path,
            "-cacheRoot", cacheRoot.path,
            "-ApplePersistenceIgnoreState", "YES",
        ]
    }

    /// A SwiftUI `Text` reports its string as the element's value.
    @MainActor
    private func waitForValue(_ element: XCUIElement, _ value: String) -> Bool {
        let matched = expectation(
            for: NSPredicate(format: "value == %@", value), evaluatedWith: element)
        return XCTWaiter().wait(for: [matched], timeout: Self.timeout) == .completed
    }

    /// One Proton account row. No `proton_accounts` row: the launch
    /// restore finds nothing to read, so the account stays `active`
    /// for the pause instead of becoming `needs_attention`.
    private static func seedProtonAccount(_ path: String) throws {
        var handle: OpaquePointer?
        let opened = sqlite3_open_v2(path, &handle, SQLITE_OPEN_READWRITE, nil)
        defer { if let handle { sqlite3_close(handle) } }
        guard opened == SQLITE_OK, let db = handle else {
            throw Self.seedFailure("open")
        }
        let sql = """
            INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('\(accountID)', 'proton', 'proton_api', '\(seededName)', 'active', 1, 0);
            """
        var error: UnsafeMutablePointer<CChar>?
        let result = sqlite3_exec(db, sql, nil, nil, &error)
        let message = error.map { String(cString: $0) } ?? "SQLite seed failed"
        sqlite3_free(error)
        if result != SQLITE_OK {
            throw Self.seedFailure(message)
        }
    }

    /// The stored display name and state of one account.
    private static func accountRow(
        _ path: String, id: String
    ) throws -> (displayName: String, state: String)? {
        var handle: OpaquePointer?
        let opened = sqlite3_open_v2(path, &handle, SQLITE_OPEN_READWRITE, nil)
        defer { if let handle { sqlite3_close(handle) } }
        guard opened == SQLITE_OK, let db = handle else {
            throw Self.seedFailure("open")
        }
        var statement: OpaquePointer?
        let prepared = sqlite3_prepare_v2(
            db, "SELECT display_name, state FROM accounts WHERE account_id = '\(id)'",
            -1, &statement, nil)
        defer { sqlite3_finalize(statement) }
        guard prepared == SQLITE_OK, let statement else {
            throw Self.seedFailure("prepare")
        }
        guard sqlite3_step(statement) == SQLITE_ROW else { return nil }
        let displayName = sqlite3_column_text(statement, 0).map { String(cString: $0) } ?? ""
        let state = sqlite3_column_text(statement, 1).map { String(cString: $0) } ?? ""
        return (displayName, state)
    }

    private static func seedFailure(_ message: String) -> NSError {
        NSError(
            domain: "AccountSettingsFlowTests", code: 1,
            userInfo: [NSLocalizedDescriptionKey: message])
    }
}
