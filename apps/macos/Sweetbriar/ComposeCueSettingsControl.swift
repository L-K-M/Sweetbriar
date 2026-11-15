import Observation
import SweetbriarBridge

/// One appearance editor's admission ticket. The shared cue owns the actual
/// query and receipt; this control prevents an old settings view from starting
/// work after restore or quit has retired its archive context.
@MainActor @Observable
final class ComposeCueSettingsControl {
    let cue: ComposeCueModel
    private let editor: ArchiveWriteTasks
    private let revision: UInt64
    private let canWrite: @MainActor () -> Bool
    private(set) var admissionError: String?

    init(
        cue: ComposeCueModel, editor: ArchiveWriteTasks, revision: UInt64,
        canWrite: @escaping @MainActor () -> Bool
    ) {
        self.cue = cue
        self.editor = editor
        self.revision = revision
        self.canWrite = canWrite
    }

    var isCurrent: Bool { editor.revision == revision }
    var writesUnavailable: Bool { !canWrite() }
    var canChoose: Bool {
        isCurrent && canWrite() && cue.mode != nil && !cue.isLoading && !cue.isSaving && !cue.isStopped
    }

    func choose(_ mode: ComposeCueMode) {
        guard isCurrent else {
            admissionError = "The archive changed. Reopen this editor before saving."
            return
        }
        guard canWrite() else {
            admissionError = "The archive is not writable. Check Backups for its status."
            return
        }
        guard cue.mode != nil, !cue.isLoading, !cue.isSaving, !cue.isStopped else {
            admissionError = "Wait for the current compose appearance operation to finish."
            return
        }
        guard cue.mode != mode else {
            admissionError = nil
            return
        }
        admissionError = nil
        if !editor.run(revision: revision, { [cue] in _ = await cue.setMode(mode) }) {
            admissionError = "The archive changed. Reopen this editor before saving."
        }
    }

    func retry() {
        guard !cue.isLoading, !cue.isSaving, !cue.isStopped else { return }
        admissionError = nil
        if !editor.run(revision: revision, { [cue] in await cue.reload() }) {
            admissionError = "The archive changed. Reopen this editor before retrying."
        }
    }
}
