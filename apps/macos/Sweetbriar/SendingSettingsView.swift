import SweetbriarBridge
import SwiftUI

/// Settings → Sending safety (spec/08 "Settings"; spec/02 "Sending"):
/// the registered Undo Send delay. Only that setting exists; there is no
/// cross-account or always-choose-identity key to register here (spec/02).
struct SendingSettingsView: View {
    let model: SendSafetySettingsModel

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
                    Text("The window Undo Send has before the message is queued: 5, 7, 10, 20 or 30 seconds.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    if model.isLoading { ProgressView("Loading sending settings…") }
                    if model.isSaving { ProgressView("Saving sending settings…") }
                    if let error = model.lastError {
                        Text(error.localizedDescription)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("sendingSettingsError")
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .task { await model.load() }
        .accessibilityIdentifier("sendingSettings")
    }
}
