import SweetbriarBridge
import Testing
@testable import Sweetbriar

/// The Settings → Accounts model (spec/08 "Settings"; spec/07
/// "Account management"; spec/10 "Notifications"): display-name
/// validation, the pause/resume button's title and enablement, the
/// re-read after a command, and a failure that keeps the last list.
@MainActor
@Suite("Accounts settings")
struct AccountsSettingsTests {
    @Test func renameValidationRefusesWithoutACall() async {
        let account = Self.account(state: .active)
        let source = FakeAccountSettingsSource(accounts: [account])
        let model = AccountsSettingsModel(source: source)
        await model.load()

        for bad in ["", "   ", String(repeating: "x", count: 129), "a\nb", "a\u{0}b"] {
            await model.rename(accountId: account.accountId, displayName: bad)
            #expect(model.commandError != nil)
            #expect(model.commandStatus == nil)
        }
        #expect(await source.calls.isEmpty, "an invalid name makes no call")

        // The bound is UTF-8 bytes, and only Cc characters are controls,
        // as in the core: a U+200D-joined emoji is a valid name.
        #expect(AccountsSettingsModel.validDisplayName(String(repeating: "é", count: 64)) != nil)
        #expect(AccountsSettingsModel.validDisplayName(String(repeating: "é", count: 65)) == nil)
        let family = "Family \u{1F468}\u{200D}\u{1F469}\u{200D}\u{1F467}"
        #expect(AccountsSettingsModel.validDisplayName(family) == family)

        await model.rename(accountId: account.accountId, displayName: "  Owner  ")
        #expect(
            await source.calls == [.update(accountId: account.accountId, displayName: "Owner")])
        #expect(model.commandError == nil)
        #expect(model.commandStatus == "Renamed")
        #expect(model.accounts.first?.displayName == "Owner")
    }

    @Test func pauseAndResumeTitlesAndEnablementFollowStateAndKind() {
        let cases: [Account] = [
            Self.account(id: "local", kind: .local, state: .active),
            Self.account(id: "proton-active", state: .active),
            Self.account(id: "proton-paused", state: .paused),
            Self.account(id: "proton-attention", state: .needsAttention),
            Self.account(id: "proton-disconnected", state: .disconnected),
            Self.account(id: "imap", kind: .imap, state: .active),
        ]
        var found: [String: Account] = [:]
        for account in cases { found[account.accountId] = account }

        func check(
            _ id: String, offersPause: Bool, title: String, canToggle: Bool,
            disconnect: Bool, signIn: Bool
        ) {
            guard let account = found[id] else {
                Issue.record("missing \(id)")
                return
            }
            #expect(AccountsSettingsModel.offersPause(account) == offersPause)
            #expect(AccountsSettingsModel.pauseTitle(for: account) == title)
            #expect(AccountsSettingsModel.canTogglePause(account) == canToggle)
            #expect(AccountsSettingsModel.offersDisconnect(account) == disconnect)
            #expect(AccountsSettingsModel.offersSignInAgain(account) == signIn)
        }

        // The local archive has no server: no pause, no disconnect, no sign-in.
        check("local", offersPause: false, title: "Pause", canToggle: false,
              disconnect: false, signIn: false)
        check("proton-active", offersPause: true, title: "Pause", canToggle: true,
              disconnect: true, signIn: true)
        check("proton-paused", offersPause: true, title: "Resume", canToggle: true,
              disconnect: true, signIn: true)
        // These need "Sign in again"; the button shows but stays disabled.
        check("proton-attention", offersPause: true, title: "Pause", canToggle: false,
              disconnect: true, signIn: true)
        check("proton-disconnected", offersPause: true, title: "Pause", canToggle: false,
              disconnect: false, signIn: true)
        check("imap", offersPause: true, title: "Pause", canToggle: true,
              disconnect: false, signIn: false)
    }

    @Test func aCommandReReadsAccounts() async {
        let account = Self.account(state: .active)
        let source = FakeAccountSettingsSource(accounts: [account])
        let model = AccountsSettingsModel(source: source)
        await model.load()
        #expect(await source.reads == 1)

        await model.togglePause(accountId: account.accountId)
        #expect(await source.reads == 2, "a command re-reads the stored list")
        #expect(model.accounts.first?.state == .paused)
        #expect(model.commandStatus == "Paused")

        await model.togglePause(accountId: account.accountId)
        #expect(await source.reads == 3)
        #expect(model.accounts.first?.state == .active)
        #expect(model.commandStatus == "Resumed")
    }

    @Test func aFailedCommandShowsItsErrorAndKeepsTheList() async {
        let account = Self.account(state: .active)
        let source = FakeAccountSettingsSource(accounts: [account])
        let model = AccountsSettingsModel(source: source)
        await model.load()
        await source.setFailure(CoreFailure.conflict(detail: "the account needs sign in again"))

        await model.togglePause(accountId: account.accountId)
        #expect(model.commandError?.contains("needs sign in again") == true)
        #expect(model.commandStatus == nil)
        #expect(model.accounts == [account])
        #expect(await source.reads == 1, "a failed command keeps the last good list")
    }

    @Test func previewsToggleWritesOnAndOff() async {
        let source = FakeAccountSettingsSource(accounts: [])
        let model = AccountsSettingsModel(source: source)
        await model.loadPreviews()
        #expect(model.previewsEnabled == false)

        await model.setPreviews(true)
        #expect(
            await source.calls == [.setSetting(key: "notifications.previews", value: "on")])
        #expect(model.previewsEnabled == true)

        await model.setPreviews(false)
        #expect(model.previewsEnabled == false)
        #expect(
            await source.calls == [
                .setSetting(key: "notifications.previews", value: "on"),
                .setSetting(key: "notifications.previews", value: "off"),
            ])
    }

    private static func account(
        id: String = "proton-1", kind: AccountKind = .proton,
        state: AccountState, name: String = "Owner"
    ) -> Account {
        Account(
            accountId: id, kind: kind, displayName: name, state: state,
            stateReason: nil, sortOrder: 1,
            username: kind == .proton ? "owner@example.org" : nil
        )
    }
}

private actor FakeAccountSettingsSource: AccountSettingsSource {
    enum Call: Equatable {
        case update(accountId: String, displayName: String)
        case pause(accountId: String)
        case resume(accountId: String)
        case disconnect(accountId: String)
        case setSetting(key: String, value: String)
    }

    private var stored: [Account]
    private(set) var calls: [Call] = []
    private(set) var reads = 0
    private var failure: (any Error)?
    private var settingsList: [Setting]

    init(accounts: [Account]) {
        stored = accounts
        settingsList = [Setting(key: "notifications.previews", value: "off", isDefault: true)]
    }

    func setFailure(_ failure: (any Error)?) { self.failure = failure }

    func accounts() async throws -> [Account] {
        reads += 1
        return stored
    }

    func updateAccount(accountId: String, displayName: String) async throws -> Receipt {
        calls.append(.update(accountId: accountId, displayName: displayName))
        if let failure { throw failure }
        stored = stored.map { account in
            guard account.accountId == accountId else { return account }
            return Account(
                accountId: account.accountId, kind: account.kind, displayName: displayName,
                state: account.state, stateReason: account.stateReason,
                sortOrder: account.sortOrder, username: account.username
            )
        }
        return Self.receipt("update_account")
    }

    func pauseAccount(accountId: String) async throws -> Receipt {
        calls.append(.pause(accountId: accountId))
        if let failure { throw failure }
        stored = stored.map { account in
            account.accountId == accountId ? Self.withState(account, .paused) : account
        }
        return Self.receipt("pause_account")
    }

    func resumeAccount(accountId: String) async throws -> Receipt {
        calls.append(.resume(accountId: accountId))
        if let failure { throw failure }
        stored = stored.map { account in
            account.accountId == accountId ? Self.withState(account, .active) : account
        }
        return Self.receipt("resume_account")
    }

    func disconnectAccount(accountId: String) async throws {
        calls.append(.disconnect(accountId: accountId))
        if let failure { throw failure }
        stored = stored.map { account in
            account.accountId == accountId ? Self.withState(account, .disconnected) : account
        }
    }

    func settings() async throws -> [Setting] { settingsList }

    func setSetting(key: String, value: String) async throws -> Receipt {
        calls.append(.setSetting(key: key, value: value))
        if let failure { throw failure }
        settingsList = settingsList.filter { $0.key != key }
            + [Setting(key: key, value: value, isDefault: false)]
        return Self.receipt("set_setting")
    }

    private static func withState(_ account: Account, _ state: AccountState) -> Account {
        Account(
            accountId: account.accountId, kind: account.kind, displayName: account.displayName,
            state: state, stateReason: account.stateReason, sortOrder: account.sortOrder,
            username: account.username
        )
    }

    private static func receipt(_ command: String) -> Receipt {
        Receipt(command: command, resultJson: "{}", createdAtMs: 0)
    }
}
