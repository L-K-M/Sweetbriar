import SweetbriarBridge
import SwiftUI

/// Settings → Sending safety (spec/08 "Settings"; spec/02 "Sending"):
/// the registered Undo Send delay. Only that setting exists; there is no
/// cross-account or always-choose-identity key to register here (spec/02).
struct SendingSettingsView: View {
    let model: SendSafetySettingsModel
    /// The recipient check's allowances (spec/08); nil hides the list.
    var allowances: RecipientAllowancesModel? = nil

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text("Sending safety").font(.title2.bold())
                Text("Undo Send holds a message before it leaves, so you can cancel it.")
                    .foregroundStyle(.secondary)
                VStack(alignment: .leading, spacing: 6) {
                    Text("Undo send delay").font(.headline)
                    Picker(
                        "Undo send delay",
                        selection: Binding(
                            get: { model.undoSeconds ?? SendSafety.fallbackUndoSeconds },
                            set: { value in Task { await model.setUndoSeconds(value) } }
                        )
                    ) {
                        ForEach(SendSafety.undoSecondsChoices, id: \.self) { seconds in
                            Text("\(seconds) seconds").tag(seconds)
                        }
                    }
                    .disabled(model.isLoading || model.isSaving || model.undoSeconds == nil)
                    .accessibilityIdentifier("sendingUndoDelay")
                    Text("How long Undo Send waits before the message is queued: \(Self.choicesText).")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    if model.isLoading { ProgressView("Loading sending settings…") }
                    if model.isSaving { ProgressView("Saving sending settings…") }
                    if let error = model.lastError {
                        Text(error.localizedDescription)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("sendingSettingsError")
                        if model.undoSeconds == nil, !model.isLoading {
                            Button("Retry") { Task { await model.load() } }
                                .accessibilityIdentifier("sendingSettingsRetry")
                        }
                    }
                }
                if let allowances { allowanceList(allowances) }
                Spacer(minLength: 0)
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .task {
            await model.load()
            await allowances?.load()
        }
        .accessibilityIdentifier("sendingSettings")
    }

    /// Addresses allowed despite the recipient check, by account, each
    /// with Remove (spec/08 "Recipient check").
    @ViewBuilder
    private func allowanceList(_ allowances: RecipientAllowancesModel) -> some View {
        Divider()
        Text("Allowed recipients").font(.headline)
        Text("Sweetbriar blocks sending to an address that has written only to your other accounts. These addresses are allowed anyway.")
            .font(.footnote)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
        if allowances.showsEmptyState {
            Text("No allowed recipients.").foregroundStyle(.secondary)
        }
        ForEach(allowances.groups, id: \.accountId) { group in
            Text(group.accountName).font(.subheadline.bold())
            ForEach(group.allowances, id: \.address) { allowance in
                HStack {
                    Text(allowance.address)
                    Spacer()
                    Button("Remove") { Task { await allowances.remove(allowance) } }
                        .disabled(allowances.removing != nil)
                        .accessibilityLabel("Remove \(allowance.address) for \(group.accountName)")
                }
            }
        }
        if let error = allowances.error {
            Text(error).foregroundStyle(.red).accessibilityIdentifier("allowancesError")
        }
    }

    /// The registered choices as the help text lists them: "5, 7, 10, 20 or 30 seconds".
    private static var choicesText: String {
        let choices = SendSafety.undoSecondsChoices.map(String.init)
        guard let last = choices.last else { return "" }
        let head = choices.dropLast().joined(separator: ", ")
        return (head.isEmpty ? last : "\(head) or \(last)") + " seconds"
    }
}
