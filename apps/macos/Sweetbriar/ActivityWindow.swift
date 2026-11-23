// The Activity window (spec/08 "Activity / account health"; plan
// M2.7d2): per-account state and condition with "Sign in again" where
// it applies, coverage, open breaker events with Accept and Keep, the
// parked and quarantined intents with Retry and Cancel, and the
// Recovery list. It observes the app's one shared `ActivityModel`;
// the model's life belongs to the main window's core session.

import AppKit
import SwiftUI
import SweetbriarBridge

/// Hosts the Activity view. One shared instance lives in `AppDelegate`;
/// closing it does not retire the model, which the main window owns.
@MainActor
final class ActivityWindowController: NSWindowController {
    static let title = "Activity"
    private static let autosaveName = "ActivityWindowFrame"
    private static let contentSize = NSSize(width: 720, height: 600)

    init(
        model: ActivityModel, bridge: CoreBridge, appearance: AppearanceModel? = nil,
        onSignInAgain: @escaping @MainActor (Account) -> Void
    ) {
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.contentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = Self.title
        // A saved frame wins; only a first open is centered.
        let restored = window.setFrameUsingName(Self.autosaveName)
        window.setFrameAutosaveName(Self.autosaveName)
        // The controller outlives a close, so the window must too.
        window.isReleasedWhenClosed = false
        window.contentView = NSHostingView(rootView: ActivityView(
            model: model, bridge: bridge, appearance: appearance, onSignInAgain: onSignInAgain
        ))
        super.init(window: window)
        if !restored { window.center() }
    }

    required init?(coder: NSCoder) {
        fatalError("ActivityWindowController is built in code")
    }
}

/// The window's content: the three sections and the two inline errors.
private struct ActivityView: View {
    let model: ActivityModel
    let bridge: CoreBridge
    /// The accounts' colors for each row's chip; nil keeps them neutral.
    let appearance: AppearanceModel?
    let onSignInAgain: @MainActor (Account) -> Void

    @State private var accounts: [Account] = []
    @State private var now = Date()
    @State private var accountsError: (any Error)?
    /// Relative times ("2 minutes ago") move on while health is quiet.
    private static let clockInterval: Duration = .seconds(30)
    private static let presentation = ActivityPresentation()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                if let loadError = model.loadError {
                    loadErrorRow(loadError)
                }
                if let accountsError {
                    Text("Couldn't read the accounts: \(accountsError.localizedDescription)")
                        .foregroundStyle(.secondary)
                }
                accountsSection
                if let actionError = model.actionError {
                    Text("The last action failed: \(actionError.localizedDescription)")
                        .foregroundStyle(.secondary)
                }
                intentsSection
                recoverySection
            }
            .padding(20)
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .task(id: model.accounts) {
            now = Date()
            do {
                let read = try await bridge.accounts()
                // A newer model change restarted this task; its read
                // supersedes this one, so this result must not land.
                guard !Task.isCancelled else { return }
                accounts = read
                accountsError = nil
            } catch is CancellationError {
            } catch {
                guard !Task.isCancelled else { return }
                // Keep the names and "Sign in again" from the last good
                // read, and say why they may be stale.
                accountsError = error
            }
        }
        .task {
            while !Task.isCancelled {
                try? await Task.sleep(for: Self.clockInterval)
                now = Date()
            }
        }
    }

    private func loadErrorRow(_ error: any Error) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text("Couldn't load Activity: \(error.localizedDescription)")
            Spacer()
            Button("Retry") { model.retry() }
        }
    }

    private var accountsSection: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Accounts").font(.headline)
            if model.accounts.isEmpty {
                Text("No accounts").foregroundStyle(.secondary)
            } else {
                ForEach(model.accounts, id: \.accountId) { health in
                    accountRow(health)
                }
            }
        }
    }

    private func accountRow(_ health: AccountHealth) -> some View {
        let name = accounts.first { $0.accountId == health.accountId }?.displayName
            ?? health.accountId
        let condition = Self.presentation.condition(
            health.condition, lastSync: health.lastSync, now: now)
        return VStack(alignment: .leading, spacing: 6) {
            AccountChip(
                label: name, accountId: health.accountId, appearance: appearance?.appearance,
                identifier: "activity-account-\(health.accountId)"
            )
            .font(.headline)
            line(condition)
            ForEach(Array(health.warnings.enumerated()), id: \.offset) { _, warning in
                Text("Couldn't unlock keys for \(warning)")
                    .foregroundStyle(.secondary)
            }
            Text("\(health.coverage.indexed) of \(health.coverage.totalMessages) indexed")
                .foregroundStyle(.secondary)
            if let proton = health.proton {
                protonCoverage(proton)
            }
            if ActivityPresentation.offersSignInAgain(health.condition),
               let account = accounts.first(where: { $0.accountId == health.accountId }) {
                Button("Sign in again") { onSignInAgain(account) }
            }
            ForEach(health.breakerEvents, id: \.eventId) { event in
                breakerRow(event)
            }
        }
    }

    /// "Proton messages synced" with its counts, and each mailbox's
    /// count against Proton's behind a disclosure (spec/04-sync-proton
    /// "Coverage").
    private func protonCoverage(_ coverage: ProtonCoverage) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            line(Self.presentation.protonCoverage(coverage))
            if !coverage.mailboxes.isEmpty {
                DisclosureGroup("Mailboxes") {
                    ForEach(coverage.mailboxes, id: \.mailboxId) { mailbox in
                        line(Self.presentation.protonMailbox(mailbox))
                    }
                }
            }
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("proton-coverage")
    }

    private func breakerRow(_ event: AccountBreakerEvent) -> some View {
        HStack(alignment: .firstTextBaseline) {
            Text(Self.presentation.breakerEvent(event))
            Spacer()
            Button("Accept") { model.resolve(eventId: event.eventId, resolution: .accept) }
                .disabled(model.resolving.contains(event.eventId))
                .accessibilityIdentifier("accept-\(event.eventId)")
            Button("Keep") { model.resolve(eventId: event.eventId, resolution: .keep) }
                .disabled(model.resolving.contains(event.eventId))
                .accessibilityIdentifier("keep-\(event.eventId)")
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("breaker-\(event.eventId)")
    }

    /// One parked or quarantined intent, with the health actions the
    /// model exposes. Both are disabled while that intent's action is
    /// in flight, so a double click can't run it twice.
    private func intentRow(_ intent: Intent) -> some View {
        let acting = model.acting.contains(intent.intentId)
        return HStack(alignment: .firstTextBaseline) {
            line(Self.presentation.intent(intent))
            Spacer()
            Button("Retry") { model.release(intentIds: [intent.intentId]) }
                .disabled(acting)
                .accessibilityIdentifier("retry-\(intent.intentId)")
            Button("Cancel") { model.cancel(intentIds: [intent.intentId]) }
                .disabled(acting)
                .accessibilityIdentifier("cancel-\(intent.intentId)")
        }
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("intent-\(intent.intentId)")
    }

    private var intentsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Intents").font(.headline)
            if model.intents.isEmpty {
                Text("No parked or quarantined changes").foregroundStyle(.secondary)
            } else {
                ForEach(model.intents, id: \.intentId) { intent in
                    intentRow(intent)
                }
            }
            if model.intentsTruncated {
                Text("More intents not shown").foregroundStyle(.secondary)
            }
        }
    }

    private var recoverySection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Recovery").font(.headline)
            if model.recovery.isEmpty {
                Text("Nothing to recover").foregroundStyle(.secondary)
            } else {
                ForEach(model.recovery, id: \.messageId) { row in
                    line(Self.presentation.recovery(row, now: now))
                }
            }
            if model.recoveryTruncated {
                Text("More recovery rows not shown").foregroundStyle(.secondary)
            }
        }
    }

    /// One row's title, then its detail under it.
    private func line(_ row: ActivityLine) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(row.title)
            if let detail = row.detail {
                Text(detail).foregroundStyle(.secondary)
            }
        }
    }
}
