// The first-run empty state (list pane): a fresh archive shows the
// unified inbox with no rows and no guidance. This island offers the
// two ways forward, both existing commands, and stays out of the way
// once an account or imported mail exists.

import SwiftUI

struct WelcomeEmptyState: View {
    var onAddAccount: @MainActor () -> Void
    var onImport: @MainActor () -> Void

    var body: some View {
        VStack(spacing: 20) {
            Image(systemName: "envelope.stack")
                .font(.system(size: 44, weight: .regular))
                .foregroundStyle(.tint)
                .accessibilityHidden(true)
            VStack(spacing: 8) {
                // The same empty-state line the list shows elsewhere;
                // here it headlines the first-run welcome.
                Text("No messages")
                    .font(.title2.bold())
                Text(
                    "Welcome to Sweetbriar. Add your Proton account to sync it, "
                        + "or import mail you already have. Nothing is sent or moved until you say so."
                )
                .font(.callout)
                .foregroundStyle(.secondary)
                .multilineTextAlignment(.center)
                .frame(maxWidth: 460)
            }
            HStack(spacing: 12) {
                Button("Add Proton Account…") { Task { @MainActor in onAddAccount() } }
                    .buttonStyle(.borderedProminent)
                    .controlSize(.large)
                    .accessibilityIdentifier("welcomeAddAccount")
                Button("Import Mail…") { Task { @MainActor in onImport() } }
                    .controlSize(.large)
                    .accessibilityIdentifier("welcomeImport")
            }
        }
        .padding(32)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
        .accessibilityElement(children: .contain)
        .accessibilityIdentifier("welcome")
    }
}
