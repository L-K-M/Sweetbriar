import XCTest
import SQLite3

/// The M2.7d2 Activity flow (spec/08 "Activity / account health"):
/// seed one Proton account and two open breaker events into a fresh
/// store, resolve one with Accept and one with Keep through Window →
/// Activity, and read the stored states back. Synthetic content only.
final class ActivityFlowTests: XCTestCase {
    private static let timeout: TimeInterval = 60
    private static let firstEvent = "proton-acct-1-1"
    private static let secondEvent = "proton-acct-1-2"

    @MainActor
    func testBreakerAcceptAndKeepLeaveTheirStoredStates() throws {
        continueAfterFailure = false
        let roots = FileManager.default.temporaryDirectory
            .appendingPathComponent("sweetbriar-activity-flow-\(UUID().uuidString)")
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
        try Self.seedBreakerEvents(store)

        let app = XCUIApplication()
        app.launchArguments = Self.launchArguments(dataRoot: dataRoot, cacheRoot: cacheRoot)
        app.launch()
        let window = app.windows["Sweetbriar"]
        XCTAssertTrue(window.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(window.outlines["sidebar"].staticTexts["Local Archive"]
            .waitForExistence(timeout: Self.timeout))

        app.menuBars.menuBarItems["Window"].click()
        app.menuBars.menuItems["Activity"].click()

        XCTAssertTrue(app.windows["Activity"].waitForExistence(timeout: Self.timeout))
        let firstRow = app.descendants(matching: .any)["breaker-\(Self.firstEvent)"]
        let secondRow = app.descendants(matching: .any)["breaker-\(Self.secondEvent)"]
        XCTAssertTrue(firstRow.waitForExistence(timeout: Self.timeout))
        XCTAssertTrue(secondRow.waitForExistence(timeout: Self.timeout))

        app.buttons["accept-\(Self.firstEvent)"].click()
        XCTAssertTrue(waitForGone(firstRow))
        app.buttons["keep-\(Self.secondEvent)"].click()
        XCTAssertTrue(waitForGone(secondRow))
        app.terminate()
        XCTAssertTrue(app.wait(for: .notRunning, timeout: Self.timeout))

        let states = try Self.breakerStates(store)
        XCTAssertEqual(states[Self.firstEvent], "accepted")
        XCTAssertEqual(states[Self.secondEvent], "kept")
    }

    private static func launchArguments(dataRoot: URL, cacheRoot: URL) -> [String] {
        [
            "-dataRoot", dataRoot.path,
            "-cacheRoot", cacheRoot.path,
            "-ApplePersistenceIgnoreState", "YES",
        ]
    }

    private func waitForGone(_ element: XCUIElement) -> Bool {
        let gone = expectation(
            for: NSPredicate(format: "exists == false"), evaluatedWith: element)
        return XCTWaiter().wait(for: [gone], timeout: Self.timeout) == .completed
    }

    /// The same rows `breakerResolutionsLeaveTheOpenListAndKeysReplay`
    /// seeds in the bridge tests.
    private static func seedBreakerEvents(_ path: String) throws {
        var handle: OpaquePointer?
        let opened = sqlite3_open_v2(path, &handle, SQLITE_OPEN_READWRITE, nil)
        defer { if let handle { sqlite3_close(handle) } }
        guard opened == SQLITE_OK, let db = handle else {
            throw Self.seedFailure("open")
        }
        let sql = """
            INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('11111111-1111-1111-1111-111111111111', 'proton', 'proton_api', 'Owner', 'active', 1, 0);
            INSERT INTO proton_accounts
             (account_id, api_base_url, username, proton_user_id, credential_ref,
              password_mode, listing_state)
             VALUES ('11111111-1111-1111-1111-111111111111', 'https://mail.example/api', 'owner', 'user-1', 'ref', 1, 'complete');
            INSERT INTO breaker_events
             (event_id, account_id, mailbox_id, kind, details_json, state, created_at)
             VALUES ('proton-acct-1-1', '11111111-1111-1111-1111-111111111111', NULL, 'bulk_disappearance',
                     '{"disappeared":1,"mailbox_messages":1}', 'open', 0);
            INSERT INTO breaker_events
             (event_id, account_id, mailbox_id, kind, details_json, state, created_at)
             VALUES ('proton-acct-1-2', '11111111-1111-1111-1111-111111111111', NULL, 'bulk_disappearance',
                     '{"disappeared":1,"mailbox_messages":1}', 'open', 0);
            """
        var error: UnsafeMutablePointer<CChar>?
        let result = sqlite3_exec(db, sql, nil, nil, &error)
        let message = error.map { String(cString: $0) } ?? "SQLite seed failed"
        sqlite3_free(error)
        if result != SQLITE_OK {
            throw Self.seedFailure(message)
        }
    }

    /// The stored state of every breaker event, by event id.
    private static func breakerStates(_ path: String) throws -> [String: String] {
        var handle: OpaquePointer?
        let opened = sqlite3_open_v2(path, &handle, SQLITE_OPEN_READWRITE, nil)
        defer { if let handle { sqlite3_close(handle) } }
        guard opened == SQLITE_OK, let db = handle else {
            throw Self.seedFailure("open")
        }
        var statement: OpaquePointer?
        let prepared = sqlite3_prepare_v2(
            db, "SELECT event_id, state FROM breaker_events", -1, &statement, nil)
        defer { sqlite3_finalize(statement) }
        guard prepared == SQLITE_OK, let statement else {
            throw Self.seedFailure("prepare")
        }
        var states: [String: String] = [:]
        while sqlite3_step(statement) == SQLITE_ROW {
            guard let event = sqlite3_column_text(statement, 0).map(String.init(cString:)),
                  let state = sqlite3_column_text(statement, 1).map(String.init(cString:))
            else { continue }
            states[event] = state
        }
        return states
    }

    private static func seedFailure(_ message: String) -> NSError {
        NSError(
            domain: "ActivityFlowTests", code: 1,
            userInfo: [NSLocalizedDescriptionKey: message])
    }
}
