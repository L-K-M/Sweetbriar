import AppKit
import SwiftUI
import UniformTypeIdentifiers

/// The diagnostics preview sheet (spec/10 "Privacy", spec/07
/// `diagnostics_package`): the package's one JSON document, read-only
/// and selectable, with Save… and Close. The core writes no file, and
/// neither does this view until the user saves; Save… writes only the
/// text, atomically and UTF-8, to the URL the user chooses.
@MainActor
struct DiagnosticsSheet: View {
    @Bindable var model: DiagnosticsModel
    @Environment(\.dismiss) private var dismiss
    @State private var saveError: String?

    /// The panel's default name (spec/10: a diagnostics export).
    private static let defaultFileName = "Sweetbriar Diagnostics.json"

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            Text("Diagnostics").font(.title2.bold())
            Text(
                "A diagnostics package explains a problem. It holds no message content, "
                    + "no log lines, no file paths and no display names."
            )
            .font(.footnote)
            .foregroundStyle(.secondary)
            Group {
                if model.isLoading {
                    ProgressView("Building the diagnostics package…")
                } else if let error = model.error {
                    Text(error)
                        .foregroundStyle(.red)
                        .accessibilityIdentifier("diagnosticsError")
                } else if let text = model.text {
                    ScrollView {
                        Text(text)
                            .font(.system(.body, design: .monospaced))
                            .textSelection(.enabled)
                            .frame(maxWidth: .infinity, alignment: .leading)
                            .padding(8)
                            .accessibilityIdentifier("diagnostics-preview")
                    }
                } else {
                    Text("No diagnostics package.").foregroundStyle(.secondary)
                }
            }
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
            if let saveError {
                Text(saveError)
                    .foregroundStyle(.red)
                    .accessibilityIdentifier("diagnosticsSaveError")
            }
            HStack {
                Spacer()
                Button("Save…") { save() }
                    .disabled(model.text == nil || model.isLoading)
                    .accessibilityIdentifier("diagnostics-save")
                Button("Close") { dismiss() }
                    .keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("diagnostics-close")
            }
        }
        .padding(24)
        .frame(minWidth: 640, minHeight: 480)
        .task { await model.load() }
    }

    /// Ask where to save, then write the text there. A failure shows;
    /// it is never silent.
    private func save() {
        guard let text = model.text else { return }
        let panel = NSSavePanel()
        panel.nameFieldStringValue = Self.defaultFileName
        panel.allowedContentTypes = [.json]
        panel.message = "Save the diagnostics package."
        panel.begin { response in
            guard response == .OK, let url = panel.url else { return }
            // Hop to the main actor rather than assume the panel's
            // completion queue: a wrong assumption would trap.
            Task { @MainActor in
                do {
                    try Data(text.utf8).write(to: url, options: .atomic)
                } catch {
                    saveError = error.localizedDescription
                }
            }
        }
    }
}
