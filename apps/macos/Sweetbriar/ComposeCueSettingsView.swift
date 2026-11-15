import SweetbriarBridge
import SwiftUI

struct ComposeCueSettingsView: View {
    @State private var control: ComposeCueSettingsControl

    init(
        cue: ComposeCueModel, editor: ArchiveWriteTasks, revision: UInt64,
        canWrite: @escaping @MainActor () -> Bool
    ) {
        _control = State(
            initialValue: ComposeCueSettingsControl(
                cue: cue, editor: editor, revision: revision, canWrite: canWrite))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Compose identity cue").font(.headline)
            Text(
                "Standard uses the identity strip. Strong also tints the upper window; the editor stays neutral."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)
            ComposeCuePicker(mode: control.cue.mode, enabled: control.canChoose) { mode in
                control.choose(mode)
            }
            if control.cue.isStopped {
                Text("Compose appearance is unavailable. Reopen Settings after the archive operation.")
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("composeCueError")
            } else if let error = control.cue.lastError {
                Text(
                    control.cue.mode == nil
                        ? "Compose appearance is unavailable: \(error.localizedDescription)"
                        : "Showing the last known compose appearance; it may be out of date: \(error.localizedDescription)"
                )
                .foregroundStyle(.red)
                .accessibilityIdentifier("composeCueError")
                if control.cue.isLoading {
                    ProgressView("Refreshing compose appearance…")
                } else {
                    Button("Retry compose appearance") { control.retry() }
                        .disabled(!control.isCurrent || control.cue.isSaving)
                        .accessibilityIdentifier("composeCueRetry")
                }
            } else if control.cue.mode == nil || control.cue.isLoading {
                ProgressView("Loading compose appearance…")
            }
            if control.cue.isSaving { ProgressView("Saving compose appearance…") }
            if !control.isCurrent {
                Text("The archive changed. Reopen Settings before making changes.").foregroundStyle(.red)
            } else if control.writesUnavailable {
                Text("The archive is not writable. Check Backups for its status.").foregroundStyle(.secondary)
            }
            if let error = control.admissionError { Text(error).foregroundStyle(.red) }
        }
        .accessibilityIdentifier("composeCueSettings")
    }
}
