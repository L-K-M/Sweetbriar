// The Activity surface across the bridge (spec/07): a fresh store
// answers empty pages; seeded Proton breaker events resolve over FFI,
// a repeated key replays its receipt, and `set_app_active` succeeds.

import Foundation
import SQLite3
import Testing
@testable import SweetbriarBridge

/// The seeded Proton account's id: a UUID, because `account_health`
/// scopes its coverage by one.
private let seededAccountID = "11111111-1111-1111-1111-111111111111"

/// Seed one Proton account and two open breaker events, the same rows
/// `breaker_events_and_the_activity_signal_cross_the_ffi` writes.
private func seedBreakerEvents(_ path: String) throws {
    var handle: OpaquePointer?
    let opened = sqlite3_open_v2(path, &handle, SQLITE_OPEN_READWRITE, nil)
    defer { if let handle { sqlite3_close(handle) } }
    try #require(opened == SQLITE_OK)
    let db = try #require(handle)
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
        throw CoreFailure.unavailable(reason: message)
    }
}

@Test
func aFreshCoreAnswersEmptyActivitySurfaces() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    let bridge = try await CoreBridge.open(
        config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
    let accounts = try await bridge.accounts()
    let health = try await bridge.accountHealth()
    // The store's one account is the local archive; a fresh store has
    // no Proton account and no warnings or open events.
    #expect(health.count <= 1)
    #expect(health.allSatisfy { $0.breakerEvents.isEmpty && $0.warnings.isEmpty })
    #expect(
        health.allSatisfy { row in accounts.contains { $0.accountId == row.accountId } })
    let intents = try await bridge.intents(
        filter: IntentFilter(accountId: nil, state: nil), after: nil, limit: 500)
    #expect(intents.rows.isEmpty && !intents.hasMore && intents.after == nil)
    let recovery = try await bridge.recovery(accountId: nil, after: nil, limit: 500)
    #expect(recovery.rows.isEmpty && !recovery.hasMore && recovery.after == nil)
    await bridge.shutdown()
}

@Test
func breakerResolutionsLeaveTheOpenListAndKeysReplay() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }

    // The schema must exist before the seed, and the seed cannot run
    // while the core holds the store's lock, so open to create it,
    // close, seed, then reopen (the compose fixture's rule).
    let opener = try await CoreBridge.open(
        config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
    await opener.shutdown()
    let db = URL(fileURLWithPath: config.dataRoot).appendingPathComponent("state.sqlite")
    try seedBreakerEvents(db.path)

    let bridge = try await CoreBridge.open(
        config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))

    let healthBefore = try await bridge.accountHealth()
    let account = try #require(healthBefore.first { $0.accountId == seededAccountID })
    #expect(account.breakerEvents.count == 2)
    #expect(account.breakerEvents.allSatisfy { $0.disappeared == 1 })
    #expect(account.breakerEvents.allSatisfy { $0.detectedAt == Date(timeIntervalSince1970: 0) })

    let accept = try await bridge.resolveBreakerEvent(
        idempotencyKey: "accept-ffi", eventId: "proton-acct-1-1", resolution: .accept)
    #expect(accept.command == "resolve_breaker_event")
    let replay = try await bridge.resolveBreakerEvent(
        idempotencyKey: "accept-ffi", eventId: "proton-acct-1-1", resolution: .keep)
    #expect(replay == accept, "a repeated key answers the stored receipt")

    let afterAccept = try await bridge.accountHealth()
    let accepted = try #require(afterAccept.first { $0.accountId == seededAccountID })
    #expect(accepted.breakerEvents.map(\.eventId) == ["proton-acct-1-2"])

    let keep = try await bridge.resolveBreakerEvent(
        idempotencyKey: "keep-ffi", eventId: "proton-acct-1-2", resolution: .keep)
    #expect(keep.command == "resolve_breaker_event")
    let afterKeep = try await bridge.accountHealth()
    let kept = try #require(afterKeep.first { $0.accountId == seededAccountID })
    #expect(kept.breakerEvents.isEmpty)

    try await bridge.setAppActive(true)
    await bridge.shutdown()
}
