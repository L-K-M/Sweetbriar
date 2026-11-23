import XCTest
import SQLite3

/// M5.2's filing flow (spec/08 "Keyboard filing") over a seeded, paused
/// Proton account: archive and undo, the palette's move and Edit →
/// Undo, ⌘⌫ left to the search field, Trash, and a refusal explained.
/// Synthetic content only; the paused account calls no server.
final class FilingFlowTests: XCTestCase {
    private static let timeout: TimeInterval = 60
    private static let account = "33333333-3333-4333-8333-333333333333"
    private static let archived = "Quarterly figures"
    private static let moved = "Lunch plans"
    private static let trashed = "Boarding pass"
    private static let scheduled = "Monday reminder"

    @MainActor
    func testFilingFromTheKeyboardUndoesAndExplains() throws {
        continueAfterFailure = false
        let roots = FileManager.default.temporaryDirectory
            .appendingPathComponent("sweetbriar-filing-flow-\(UUID().uuidString)")
        let dataRoot = roots.appendingPathComponent("data")
        let cacheRoot = roots.appendingPathComponent("cache")
        defer { try? FileManager.default.removeItem(at: roots) }
        let store = dataRoot.appendingPathComponent("state.sqlite").path
        let arguments = [
            "-dataRoot", dataRoot.path,
            "-cacheRoot", cacheRoot.path,
            "-ApplePersistenceIgnoreState", "YES",
        ]

        // The first launch creates the store; seed it with no core open.
        let opener = XCUIApplication()
        opener.launchArguments = arguments
        opener.launch()
        XCTAssertTrue(opener.windows["Sweetbriar"].outlines["sidebar"].staticTexts["Local Archive"]
            .waitForExistence(timeout: Self.timeout))
        opener.terminate()
        XCTAssertTrue(opener.wait(for: .notRunning, timeout: Self.timeout))
        try Self.seed(store)

        let app = XCUIApplication()
        app.launchArguments = arguments
        app.launch()
        let window = app.windows["Sweetbriar"]
        let rows = window.tables["messageList"].tableRows
        waitUntil(rows, "count == 3")

        // Archive, then the toast's Undo.
        select(Self.archived, in: window)
        app.typeKey("a", modifierFlags: [.command, .control])
        waitUntil(rows, "count == 2")
        let toast = window.staticTexts["filingToastMessage"]
        XCTAssertTrue(toast.waitForExistence(timeout: Self.timeout))
        window.buttons["filingUndo"].click()
        waitUntil(rows, "count == 3")

        // The palette, then Edit → Undo's key with the list focused.
        select(Self.moved, in: window)
        move(to: "wor", app: app)
        waitUntil(rows, "count == 2")
        app.typeKey("z", modifierFlags: .command)
        waitUntil(rows, "count == 3")
        select(Self.moved, in: window)
        move(to: "wor", app: app)
        waitUntil(rows, "count == 2")

        // In the search field ⌘⌫ edits the text and files nothing.
        let field = window.searchFields["search.field"]
        field.click()
        field.typeText("boarding")
        field.typeKey(.delete, modifierFlags: .command)
        waitUntil(field, "value == ''")
        waitUntil(rows, "count == 2")

        select(Self.trashed, in: window)
        app.typeKey(.delete, modifierFlags: .command)
        waitUntil(rows, "count == 1")

        // A refusal is explained and changes nothing.
        window.outlines["sidebar"].staticTexts["Scheduled"].click()
        XCTAssertTrue(row(Self.scheduled, in: window).waitForExistence(timeout: Self.timeout))
        select(Self.scheduled, in: window)
        app.typeKey(.delete, modifierFlags: .command)
        let sheet = window.sheets.firstMatch
        XCTAssertTrue(sheet.staticTexts["Messages in Scheduled can't be filed."]
            .waitForExistence(timeout: Self.timeout))
        sheet.buttons["OK"].click()
        waitUntil(rows, "count == 1")

        app.terminate()
        XCTAssertTrue(app.wait(for: .notRunning, timeout: Self.timeout))
        XCTAssertEqual(
            try Self.mailboxes(store),
            [1: ["Inbox"], 2: ["Work"], 3: ["Trash"], 4: ["Scheduled"]])
    }

    @MainActor
    private func move(to typed: String, app: XCUIApplication) {
        app.typeKey("m", modifierFlags: [.command, .shift])
        let field = app.searchFields["filingPaletteField"]
        XCTAssertTrue(field.waitForExistence(timeout: Self.timeout))
        field.typeText(typed)
        field.typeKey(.return, modifierFlags: [])
        waitUntil(field, "exists == false")
    }

    @MainActor
    private func row(_ subject: String, in window: XCUIElement) -> XCUIElement {
        window.tables["messageList"].descendants(matching: .any)
            .matching(NSPredicate(format: "label CONTAINS %@", subject)).firstMatch
    }

    @MainActor
    private func select(_ subject: String, in window: XCUIElement) {
        let row = row(subject, in: window)
        XCTAssertTrue(row.waitForExistence(timeout: Self.timeout))
        row.click()
    }

    @MainActor
    private func waitUntil(_ object: Any, _ format: String) {
        let met = expectation(for: NSPredicate(format: format), evaluatedWith: object)
        XCTAssertEqual(XCTWaiter().wait(for: [met], timeout: Self.timeout), .completed, format)
    }

    /// Three messages in Inbox and one in Scheduled, each also listed.
    private static func seed(_ path: String) throws {
        try execute(path, """
            BEGIN;
            INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('\(account)', 'proton', 'proton_api', 'Personal', 'paused', 1, 0);
            INSERT INTO proton_accounts
             (account_id, api_base_url, username, proton_user_id, credential_ref, password_mode,
              listing_state)
             VALUES ('\(account)', 'https://127.0.0.1:1', 'synthetic', 'user-1', 'ref', 1, 'none');
            INSERT INTO account_appearance (account_id, color_srgb, symbol, assignment, revision)
             VALUES ('\(account)', '#0072b2', NULL, 'palette:v1:0', 1);
            INSERT INTO mailboxes
             (mailbox_id, account_id, provider_key, remote_name, role, kind, selectable, sync_state)
             VALUES (101, '\(account)', '0', 'Inbox', 'inbox', 'folder', 1, 'synced'),
                    (102, '\(account)', '6', 'Archive', 'archive', 'folder', 1, 'synced'),
                    (103, '\(account)', '3', 'Trash', 'trash', 'folder', 1, 'synced'),
                    (104, '\(account)', 'folder-work', 'Work', NULL, 'folder', 1, 'synced'),
                    (105, '\(account)', '12', 'Scheduled', NULL, 'virtual', 1, 'synced');
            INSERT INTO logical_messages (message_id, account_id, provider_key, created_at)
             VALUES (1, '\(account)', 'msg-1', 0), (2, '\(account)', 'msg-2', 0),
                    (3, '\(account)', 'msg-3', 0), (4, '\(account)', 'msg-4', 0);
            INSERT INTO occurrences
             (occurrence_id, account_id, mailbox_id, message_id, state, first_seen_at, last_seen_at)
             VALUES (11, '\(account)', 101, 1, 'present', 0, 0),
                    (12, '\(account)', 101, 2, 'present', 0, 0),
                    (13, '\(account)', 101, 3, 'present', 0, 0),
                    (14, '\(account)', 105, 4, 'present', 0, 0);
            INSERT INTO message_headers
             (message_id, raw_fields, subject, from_json, date_header, fetched_at)
             VALUES (1, x'', '\(archived)', '[{"address":"a@example.test"}]', 3000, 0),
                    (2, x'', '\(moved)', '[{"address":"b@example.test"}]', 2000, 0),
                    (3, x'', '\(trashed)', '[{"address":"c@example.test"}]', 1000, 0),
                    (4, x'', '\(scheduled)', '[{"address":"d@example.test"}]', 4000, 0);
            INSERT INTO d_list_rows
             VALUES (11, 101, 'inbox', '\(account)', 3000, 1, 'a@example.test', '\(archived)', NULL, 0, 0, 0, 1),
                    (12, 101, 'inbox', '\(account)', 2000, 2, 'b@example.test', '\(moved)', NULL, 0, 0, 0, 1),
                    (13, 101, 'inbox', '\(account)', 1000, 3, 'c@example.test', '\(trashed)', NULL, 0, 0, 0, 1),
                    (14, 105, NULL, '\(account)', 4000, 4, 'd@example.test', '\(scheduled)', NULL, 0, 0, 0, 1);
            COMMIT;
            """)
    }

    /// Where each message is listed, by message id.
    private static func mailboxes(_ path: String) throws -> [Int64: [String]] {
        try withDatabase(path) { db in
            var statement: OpaquePointer?
            let sql = """
                SELECT o.message_id, m.remote_name FROM occurrences o
                 JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
                 WHERE o.state IN ('present', 'vanish_pending') ORDER BY m.remote_name
                """
            let prepared = sqlite3_prepare_v2(db, sql, -1, &statement, nil)
            defer { sqlite3_finalize(statement) }
            guard prepared == SQLITE_OK, let statement else { throw failure("prepare") }
            var found: [Int64: [String]] = [:]
            while sqlite3_step(statement) == SQLITE_ROW {
                guard let name = sqlite3_column_text(statement, 1).map(String.init(cString:))
                else { continue }
                found[sqlite3_column_int64(statement, 0), default: []].append(name)
            }
            return found
        }
    }

    private static func execute(_ path: String, _ sql: String) throws {
        try withDatabase(path) { db in
            var error: UnsafeMutablePointer<CChar>?
            let result = sqlite3_exec(db, sql, nil, nil, &error)
            let message = error.map { String(cString: $0) } ?? "SQLite seed failed"
            sqlite3_free(error)
            guard result == SQLITE_OK else { throw failure(message) }
        }
    }

    private static func withDatabase<T>(_ path: String, _ body: (OpaquePointer) throws -> T) throws -> T {
        var handle: OpaquePointer?
        let opened = sqlite3_open_v2(path, &handle, SQLITE_OPEN_READWRITE, nil)
        defer { if let handle { sqlite3_close(handle) } }
        guard opened == SQLITE_OK, let db = handle else { throw failure("open") }
        return try body(db)
    }

    private static func failure(_ message: String) -> NSError {
        NSError(
            domain: "FilingFlowTests", code: 1,
            userInfo: [NSLocalizedDescriptionKey: message])
    }
}
