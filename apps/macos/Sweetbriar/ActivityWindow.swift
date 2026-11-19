// The Activity window (spec/08 "Activity / account health"; plan
// M2.7d2): per-account state and condition with "Sign in again" where
// it applies, coverage, open breaker events with Accept and Keep, the
// parked and quarantined intents (read only until M3.1) and the
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
        model: ActivityModel, bridge: CoreBridge,
        onSignInAgain: @escaping @MainActor (Account) -> Void
    ) {
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.contentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = Self.title
        window.setFrameAutosaveName(Self.autosaveName)
        // The controller outlives a close, so the window must too.
        window.isReleasedWhenClosed = false
        window.contentView = NSHostingView(rootView: ActivityView(
            model: model, bridge: bridge, onSignInAgain: onSignInAgain
        ))
        super.init(window: window)
        window.center()
    }

    required init?(coder: NSCoder) {
        fatalError("ActivityWindowController is built in code")
    }
}

/// The window's content: the three sections and the two inline errors.
private struct ActivityView: View {
    let model: ActivityModel
    let bridge: CoreBridge
    let onSignInAgain: @MainActor (Account) -> Void

    @State private var accounts: [Account] = []
    @State private var now = Date()
    private static let presentation = ActivityPresentation()

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 20) {
                if let loadError = model.loadError {
                    loadErrorRow(loadError)
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
            accounts = (try? await bridge.accounts()) ?? []
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
            Text(name).font(.headline)
            line(condition)
            ForEach(Array(health.warnings.enumerated()), id: \.offset) { _, warning in
                Text("Couldn't unlock keys for \(warning)")
                    .foregroundStyle(.secondary)
            }
            Text("\(health.coverage.indexed) of \(health.coverage.totalMessages) indexed")
                .foregroundStyle(.secondary)
            if ActivityPresentation.offersSignInAgain(health.condition),
               let account = accounts.first(where: { $0.accountId == health.accountId }) {
                Button("Sign in again") { onSignInAgain(account) }
            }
            ForEach(health.breakerEvents, id: \.eventId) { event in
                breakerRow(event)
            }
        }
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

    private var intentsSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Intents").font(.headline)
            if model.intents.isEmpty {
                Text("No parked or quarantined changes").foregroundStyle(.secondary)
            } else {
                ForEach(model.intents, id: \.intentId) { intent in
                    line(Self.presentation.intent(intent))
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
