import Foundation
import Observation
import SweetbriarBridge

/// The Settings → Accounts tab's logic (spec/08 "Settings"; spec/07
/// "Account management"; spec/10 "Notifications"): the stored account
/// list, the rename, pause/resume and disconnect commands, and the
/// notification previews setting. It talks to the core through
/// `AccountSettingsSource`, so tests substitute a fake.
@MainActor
@Observable
final class AccountsSettingsModel {
    /// The stored accounts, by sort order then id.
    private(set) var accounts: [Account] = []
    /// True while a list read runs; the view shows a spinner only
    /// until the first list lands.
    private(set) var isLoading = false
    /// Why the list read failed; the last good list stays shown.
    private(set) var loadError: String?
    /// The last command's failure, shown until the next command.
    private(set) var commandError: String?
    /// The last command's short success text, shown until the next.
    private(set) var commandStatus: String?
    /// The account a command is running for; its buttons disable.
    private(set) var busyAccountId: String?
    /// `notifications.previews` (spec/10): off unless the user turns it
    /// on; the read setting confirms it.
    private(set) var previewsEnabled = false
    private(set) var previewsError: String?

    private let source: any AccountSettingsSource

    init(source: any AccountSettingsSource) {
        self.source = source
    }

    /// spec/10's previews setting key and its two values.
    static let previewsSettingKey = "notifications.previews"
    private static let previewsOn = "on"
    private static let previewsOff = "off"

    /// spec/07's display-name bound, in UTF-8 bytes once trimmed.
    static let maxDisplayNameBytes = 128

    /// The stored form of a display name (spec/07): trimmed, 1–128
    /// UTF-8 bytes, no control characters; nil refuses the name
    /// without a call. "Control" is the core's `char::is_control`,
    /// general category Cc only: `CharacterSet.controlCharacters` also
    /// holds the Cf format characters, so it would refuse a name the
    /// core stores, such as an emoji joined by U+200D.
    static func validDisplayName(_ raw: String) -> String? {
        let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed.utf8.count <= maxDisplayNameBytes else { return nil }
        guard !trimmed.unicodeScalars.contains(where: { $0.properties.generalCategory == .control })
        else { return nil }
        return trimmed
    }

    /// The local archive has no server, so the UI offers it no
    /// pause/resume (spec/07).
    static func offersPause(_ account: Account) -> Bool { account.kind != .local }

    /// One button's title follows the state: an active account pauses,
    /// a paused one resumes. A `needs_attention` or `disconnected`
    /// account is not paused, so it shows "Pause" disabled.
    static func pauseTitle(for account: Account) -> String {
        account.state == .paused ? "Resume" : "Pause"
    }

    /// Only an active or paused server account can toggle (spec/07):
    /// `needs_attention` and `disconnected` need "Sign in again".
    static func canTogglePause(_ account: Account) -> Bool {
        account.kind != .local && (account.state == .active || account.state == .paused)
    }

    /// Only a Proton account that is not already disconnected
    /// (spec/07); the local archive and IMAP accounts are refused.
    static func offersDisconnect(_ account: Account) -> Bool {
        account.kind == .proton && account.state != .disconnected
    }

    /// Proton accounts always offer "Sign in again" (spec/08); it is
    /// essential once the account needs attention or is disconnected.
    static func offersSignInAgain(_ account: Account) -> Bool {
        account.kind == .proton
    }

    /// Read the account list and the previews setting.
    func load() async {
        isLoading = true
        loadError = nil
        defer { isLoading = false }
        do {
            accounts = try await source.accounts()
        } catch {
            loadError = error.localizedDescription
        }
        // The previews toggle reads its own setting, so a failed list
        // read doesn't leave it showing a default.
        await loadPreviews()
    }

    /// Re-read the previews setting; a failure keeps the last value.
    func loadPreviews() async {
        previewsError = nil
        do {
            let settings = try await source.settings()
            guard let setting = settings.first(where: { $0.key == Self.previewsSettingKey }) else {
                throw CoreFailure.internalError(
                    detail: "The \(Self.previewsSettingKey) setting is missing.")
            }
            switch setting.value {
            case Self.previewsOn: previewsEnabled = true
            case Self.previewsOff: previewsEnabled = false
            default:
                throw CoreFailure.internalError(
                    detail: "The \(Self.previewsSettingKey) setting is neither on nor off.")
            }
        } catch {
            previewsError = error.localizedDescription
        }
    }

    /// Write the previews setting. The toggle snaps back on failure.
    func setPreviews(_ enabled: Bool) async {
        previewsError = nil
        let value = enabled ? Self.previewsOn : Self.previewsOff
        do {
            _ = try await source.setSetting(key: Self.previewsSettingKey, value: value)
            previewsEnabled = enabled
        } catch {
            previewsError = error.localizedDescription
        }
    }

    /// Rename an account. An invalid name is refused here, before a
    /// call; the core's `InvalidInput` still surfaces if it refuses.
    func rename(accountId: String, displayName raw: String) async {
        guard let name = Self.validDisplayName(raw) else {
            commandError = "Enter a name of 1 to 128 bytes with no control characters."
            commandStatus = nil
            return
        }
        await run(accountId: accountId, status: "Renamed") {
            _ = try await self.source.updateAccount(accountId: accountId, displayName: name)
        }
    }

    /// Pause an active account, resume a paused one.
    func togglePause(accountId: String) async {
        guard let account = accounts.first(where: { $0.accountId == accountId }) else {
            commandError = "That account is no longer listed."
            commandStatus = nil
            return
        }
        guard Self.canTogglePause(account) else {
            commandError = "Only an active or paused account can be paused or resumed."
            commandStatus = nil
            return
        }
        let pausing = account.state != .paused
        await run(accountId: accountId, status: pausing ? "Paused" : "Resumed") {
            if pausing {
                _ = try await self.source.pauseAccount(accountId: accountId)
            } else {
                _ = try await self.source.resumeAccount(accountId: accountId)
            }
        }
    }

    /// Disconnect a Proton account, keeping its rows and local mail.
    func disconnect(accountId: String) async {
        await run(accountId: accountId, status: "Disconnected") {
            try await self.source.disconnectAccount(accountId: accountId)
        }
    }

    /// Run one command under the busy flag, then re-read the list so it
    /// shows the stored state. A failure keeps the last good list and
    /// shows the error.
    private func run(
        accountId: String, status: String, _ work: @escaping @MainActor () async throws -> Void
    ) async {
        guard busyAccountId == nil else {
            commandError = "Wait for the current change to finish."
            commandStatus = nil
            return
        }
        busyAccountId = accountId
        commandError = nil
        commandStatus = nil
        defer { busyAccountId = nil }
        do {
            try await work()
        } catch is CancellationError {
            commandError = "The command was interrupted. Check the account's state."
            return
        } catch {
            commandError = error.localizedDescription
            return
        }
        do {
            accounts = try await source.accounts()
        } catch {
            // The command landed; only the list is stale.
            commandError = "\(status), but the account list didn't refresh: "
                + error.localizedDescription
            return
        }
        // The status lands only once the stored list has replaced the
        // old one, so a control re-enabled by it is safe to use.
        commandStatus = status
    }
}
