import AppKit
import SweetbriarBridge
import SwiftUI

/// The same health/model as Settings, including a second finding event when
/// its evidence copy is published. Paths are copyable, never opened or deleted.
@MainActor
struct IntegrityBanner: View {
    let model: BackupsModel
    let openBackups: @MainActor () -> Void

    var body: some View {
        if model.isClosed {
            notice("Archive closed", detail: "Quit and reopen Sweetbriar to settle the restore.")
        } else if case .readOnly(let detail, let detectedAt, let copyPath) = model.health?.integrity {
            VStack(alignment: .leading, spacing: 6) {
                notice("Archive is read-only", detail: detail)
                if let detectedAt {
                    Text("Detected: \(detectedAt.formatted(date: .abbreviated, time: .standard))")
                        .font(.caption)
                }
                if let copyPath {
                    HStack(alignment: .top) {
                        Text("Evidence copy: \(copyPath)").textSelection(.enabled)
                            .accessibilityIdentifier("integrityCopyPath")
                        Button("Copy path") { copy(copyPath) }
                            .accessibilityIdentifier("integrityCopyPathButton")
                    }
                    .font(.caption)
                } else {
                    Text("An evidence copy is not available yet.").font(.caption)
                }
            }
            .padding(12)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(.yellow.opacity(0.14))
            .accessibilityIdentifier("integrityBanner")
        } else if let error = model.healthError {
            notice("Archive health unavailable", detail: error.localizedDescription)
                .padding(12)
                .accessibilityIdentifier("integrityBanner")
        }
    }

    private func notice(_ title: String, detail: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline)
                Text(detail).textSelection(.enabled)
            }
            Spacer(minLength: 8)
            Button("Backups…", action: openBackups).accessibilityIdentifier("openBackups")
        }
    }

    private func copy(_ path: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(path, forType: .string)
    }
}
