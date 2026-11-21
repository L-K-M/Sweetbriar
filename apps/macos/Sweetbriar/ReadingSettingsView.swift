import SweetbriarBridge
import SwiftUI

/// Settings → Reading (spec/08 "Reader" Marking read; spec/02
/// `reader.mark_read`): when the reader marks a shown unread Proton
/// message read.
struct ReadingSettingsView: View {
    let model: ReadingSettingsModel

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text("Reading").font(.title2.bold())
                VStack(alignment: .leading, spacing: 6) {
                    Text("Mark messages as read").font(.headline)
                    Picker(
                        "Mark messages as read",
                        selection: Binding(
                            get: { model.markRead?.value ?? MarkRead.fallback.value },
                            set: { value in
                                guard let choice = MarkRead(value: value) else { return }
                                Task { await model.setMarkRead(choice) }
                            }
                        )
                    ) {
                        ForEach(MarkRead.choices, id: \.value) { choice in
                            Text(choice.title).tag(choice.value)
                        }
                    }
                    .disabled(model.isLoading || model.isSaving || model.markRead == nil)
                    .accessibilityIdentifier("readingMarkRead")
                    Text("When a Proton message you open is marked read. Manually leaves it unread until you mark it with ⇧⌘U.")
                        .font(.footnote)
                        .foregroundStyle(.secondary)
                    if model.isLoading { ProgressView("Loading reading settings…") }
                    if model.isSaving { ProgressView("Saving reading settings…") }
                    if let error = model.lastError {
                        Text(error.localizedDescription)
                            .foregroundStyle(.red)
                            .accessibilityIdentifier("readingSettingsError")
                        if model.markRead == nil, !model.isLoading {
                            Button("Retry") { Task { await model.load() } }
                                .accessibilityIdentifier("readingSettingsRetry")
                        }
                    }
                }
                Spacer(minLength: 0)
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .task { await model.load() }
        .accessibilityIdentifier("readingSettings")
    }
}
