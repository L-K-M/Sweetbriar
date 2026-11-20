// Account settings across the L1 boundary (spec/07 "Account
// management", spec/10 "Notifications"): the stored account list, the
// rename, pause, resume and disconnect commands, and the
// `notifications.previews` setting. App code never imports
// `SweetbriarCore`.

import Foundation

/// The account settings calls the tab needs (spec/07, spec/10).
/// `CoreBridge` adopts it; tests substitute a fake.
public protocol AccountSettingsSource: Sendable {
    func accounts() async throws -> [Account]
    func updateAccount(accountId: String, displayName: String) async throws -> Receipt
    func pauseAccount(accountId: String) async throws -> Receipt
    func resumeAccount(accountId: String) async throws -> Receipt
    /// The existing Proton sign-in disconnect; it discards the receipt.
    func disconnectAccount(accountId: String) async throws
    func settings() async throws -> [Setting]
    func setSetting(key: String, value: String) async throws -> Receipt
}
