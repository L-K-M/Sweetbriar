import AppKit
import SweetbriarBridge
import SwiftUI

struct ComposeCueSettingsView: View {
    /// What the previews draw without a resolved account.
    static let noAccount = IdentityStripView.Content.unavailable(label: "Without a sending identity")

    @Environment(\.colorSchemeContrast) private var contrast
    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @State private var control: ComposeCueSettingsControl
    private let preview: IdentityStripView.Content
    private let previewLogo: NSImage?

    init(
        cue: ComposeCueModel, editor: ArchiveWriteTasks, revision: UInt64,
        preview: IdentityStripView.Content = ComposeCueSettingsView.noAccount,
        previewLogo: NSImage? = nil,
        canWrite: @escaping @MainActor () -> Bool
    ) {
        self.preview = preview
        self.previewLogo = previewLogo
        _control = State(
            initialValue: ComposeCueSettingsControl(
                cue: cue, editor: editor, revision: revision, canWrite: canWrite))
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Compose identity cue").font(.headline)
            Text(
                "Standard colors the identity strip. Strong also tints the upper window. Color Wash extends that color behind the text and uses Strong when Increase Contrast or Reduce Transparency is on. Minimal colors only the monogram. The full From address shows in every mode."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)
            .fixedSize(horizontal: false, vertical: true)
            ComposeCuePicker(
                mode: control.cue.mode, enabled: control.canChoose, preview: preview, previewLogo: previewLogo,
                displayOptions: (contrast == .increased, reduceTransparency)
            ) { mode in
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
