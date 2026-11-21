import AppKit
import SweetbriarBridge

/// Hands a file to the app that opens its type (spec/08 "External
/// editor"); tests inject a fake editor.
@MainActor
protocol ExternalEditorOpening {
    /// The name of the default app for `file`; nil when no app opens it.
    func applicationName(for file: URL) -> String?
    /// Open `file` in that app.
    func open(_ file: URL) async throws
}

/// The system's default app for the file's type.
struct WorkspaceEditorOpener: ExternalEditorOpening {
    func applicationName(for file: URL) -> String? {
        guard let app = NSWorkspace.shared.urlForApplication(toOpen: file) else { return nil }
        let name = FileManager.default.displayName(atPath: app.path)
        return name.hasSuffix(".app") ? String(name.dropLast(4)) : name
    }

    func open(_ file: URL) async throws {
        guard let app = NSWorkspace.shared.urlForApplication(toOpen: file) else {
            throw CoreFailure.unavailable(reason: "No app can open \(file.lastPathComponent).")
        }
        _ = try await NSWorkspace.shared.open(
            [file], withApplicationAt: app, configuration: NSWorkspace.OpenConfiguration())
    }
}

/// Where a compose window's external edits go and what opens them.
struct ExternalEditor {
    let root: URL
    let opener: any ExternalEditorOpening
}

/// One body out in an external editor.
@MainActor
final class ExternalEdit {
    let watch: ExternalEditWatch
    let appName: String
    var isFinishing = false

    init(watch: ExternalEditWatch, appName: String) {
        self.watch = watch
        self.appName = appName
    }
}
