import SwiftUI
import SweetbriarBridge

/// Settings → Accounts (spec/08 "Settings"; spec/07 "Account
/// management"): one row per stored account with its name, state and
/// server commands, plus the notification previews toggle (spec/10).
struct AccountsSettingsView: View {
    let model: AccountsSettingsModel
    let editor: ArchiveWriteTasks
    let editorRevision: UInt64
    let onSignInAgain: @MainActor (Account) -> Void

    @State private var renaming: [String: String] = [:]
    @State private var disconnectCandidate: Account?
    @State private var confirmDisconnect = false
    @State private var admissionError: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text("Accounts").font(.title2.bold())
                if let loadError = model.loadError {
                    Text(loadError).foregroundStyle(.red)
                        .accessibilityIdentifier("accountLoadError")
                }
                if model.isLoading && model.accounts.isEmpty {
                    ProgressView("Loading accounts…")
                } else if model.accounts.isEmpty {
                    Text("No accounts available.").foregroundStyle(.secondary)
                } else {
                    ForEach(model.accounts, id: \.accountId) { account in
                        accountRow(account)
                    }
                }
                if let commandError = model.commandError {
                    Text(commandError).foregroundStyle(.red)
                        .accessibilityIdentifier("accountCommandError")
                }
                if let commandStatus = model.commandStatus {
                    Text(commandStatus).foregroundStyle(.secondary)
                        .accessibilityIdentifier("accountCommandStatus")
                }
                if let admissionError {
                    Text(admissionError).foregroundStyle(.red)
                        .accessibilityIdentifier("accountAdmissionError")
                }
                Divider()
                previewsSection
                Spacer(minLength: 0)
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .task { await model.load() }
        .confirmationDialog(
            "Disconnect \(disconnectCandidate?.displayName ?? "this account")?",
            isPresented: $confirmDisconnect,
            presenting: disconnectCandidate
        ) { account in
            Button("Disconnect", role: .destructive) {
                run { await model.disconnect(accountId: account.accountId) }
            }
            Button("Cancel", role: .cancel) {}
        } message: { account in
            Text(
                "Sweetbriar stops syncing \(account.displayName). "
                    + "Mail already on this device stays."
            )
        }
    }

    private func accountRow(_ account: Account) -> some View {
        VStack(alignment: .leading, spacing: 6) {
            Text(account.displayName).font(.headline)
            Text(stateText(account))
                .foregroundStyle(.secondary)
                .accessibilityIdentifier("account-state-\(account.accountId)")
            if let reason = account.stateReason, !reason.isEmpty {
                Text(reason).font(.footnote).foregroundStyle(.secondary)
            }
            if let username = account.username, !username.isEmpty {
                Text(username).font(.footnote).foregroundStyle(.secondary)
            }
            HStack(spacing: 8) {
                TextField(
                    "Display name",
                    text: Binding(
                        get: { renaming[account.accountId] ?? account.displayName },
                        set: { renaming[account.accountId] = $0 }
                    )
                )
                .accessibilityIdentifier("account-name-\(account.accountId)")
                Button("Rename") {
                    run {
                        await model.rename(
                            accountId: account.accountId,
                            displayName: renaming[account.accountId] ?? account.displayName
                        )
                    }
                }
                .disabled(model.busyAccountId != nil)
                .accessibilityIdentifier("account-rename-\(account.accountId)")
            }
            HStack(spacing: 8) {
                if AccountsSettingsModel.offersPause(account) {
                    Button(AccountsSettingsModel.pauseTitle(for: account)) {
                        run { await model.togglePause(accountId: account.accountId) }
                    }
                    .disabled(
                        !AccountsSettingsModel.canTogglePause(account)
                            || model.busyAccountId != nil
                    )
                    .accessibilityIdentifier("account-pause-\(account.accountId)")
                }
                if AccountsSettingsModel.offersDisconnect(account) {
                    Button("Disconnect") {
                        disconnectCandidate = account
                        confirmDisconnect = true
                    }
                    .disabled(model.busyAccountId != nil)
                    .accessibilityIdentifier("account-disconnect-\(account.accountId)")
                }
                if AccountsSettingsModel.offersSignInAgain(account) {
                    Button("Sign in again") { onSignInAgain(account) }
                        .accessibilityIdentifier("account-signin-\(account.accountId)")
                }
            }
        }
        .padding(10)
        .background(RoundedRectangle(cornerRadius: 6).stroke(.secondary))
    }

    private var previewsSection: some View {
        VStack(alignment: .leading, spacing: 6) {
            Text("Notifications").font(.headline)
            Toggle(
                "Show sender and subject in notifications",
                isOn: Binding(
                    get: { model.previewsEnabled },
                    set: { value in run { await model.setPreviews(value) } }
                )
            )
            .accessibilityIdentifier("notifications-previews")
            Text("Off names only the account.")
                .font(.footnote)
                .foregroundStyle(.secondary)
            if let error = model.previewsError {
                Text(error).foregroundStyle(.red)
                    .accessibilityIdentifier("accountPreviewsError")
            }
        }
    }

    private func stateText(_ account: Account) -> String {
        switch account.state {
        case .active: return "Active"
        case .paused: return "Paused"
        case .needsAttention: return "Needs attention"
        case .disconnected: return "Disconnected"
        }
    }

    /// Admit a write through the window's editor, so a restore or a
    /// closed archive cannot be written against.
    private func run(_ work: @escaping @MainActor () async -> Void) {
        guard editor.run(revision: editorRevision, work) else {
            admissionError = "The archive changed. Reopen Settings before making changes."
            return
        }
        admissionError = nil
    }
}
