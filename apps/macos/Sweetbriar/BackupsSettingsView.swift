// Settings → Backups (plan M1.8j6b; spec/08). The shared bridge-owned
// BackupsModel drives this pane and the main-window banner alike: the
// view issues commands and polls the rows, but owns no core work,
// inspects no filesystem paths and never cancels the model's calls.

import AppKit
import SweetbriarBridge
import SwiftUI

@MainActor
struct BackupsSettingsView: View {
    @Bindable var model: BackupsModel
    /// Why the shell refuses a restore right now; nil when it can run.
    let restoreBlockedReason: String?
    /// The shell's guarded confirm; false means it refused to start.
    let confirmRestore: @MainActor () -> Bool

    @State private var confirmError: String?

    var body: some View {
        ScrollView {
            VStack(alignment: .leading, spacing: 12) {
                Text("Backups").font(.title2.bold())
                statusSection
                destinationSection
                actionRow
                generationsSection
                artifactsSection
                Spacer(minLength: 0)
            }
            .padding(24)
            .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
        }
        .task {
            // Task cancellation ends only the poll; the model owns its work.
            while !Task.isCancelled && !model.isClosed {
                await model.refresh()
                try? await Task.sleep(for: .seconds(1))
            }
        }
        .sheet(isPresented: confirmationPresented) { confirmationSheet }
    }

    @ViewBuilder private var statusSection: some View {
        if model.isClosed {
            Text("The archive core is closed. Quit and reopen Sweetbriar.").foregroundStyle(.red)
        }
        if case .readOnly(let detail, let detectedAt, let copyPath) = model.health?.integrity {
            VStack(alignment: .leading, spacing: 4) {
                Text("The archive is read-only: \(detail)").foregroundStyle(.red)
                if let detectedAt {
                    Text("Detected \(detectedAt.formatted(date: .abbreviated, time: .shortened))")
                        .font(.footnote).foregroundStyle(.secondary)
                }
                if let copyPath {
                    Text(copyPath).font(.footnote).textSelection(.enabled)
                }
            }
        }
        if let healthError = model.healthError {
            Text(BackupsPresentation.error(healthError)).foregroundStyle(.red)
        }
        if let readError = model.readError {
            Text(BackupsPresentation.error(readError))
                .foregroundStyle(.red)
                .accessibilityIdentifier("backupReadError")
        }
        if let commandError = model.commandError {
            Text(BackupsPresentation.error(commandError)).foregroundStyle(.red)
        }
        if let activity = model.activity {
            HStack(spacing: 8) {
                ProgressView().controlSize(.small)
                Text(BackupsPresentation.activity(activity))
                if case .restoring = activity {
                    Button("Cancel Restore") { model.cancelRestore() }
                        .accessibilityIdentifier("backupCancelRestore")
                }
            }
        }
        if let result = model.lastResult {
            Text(BackupsPresentation.result(result)).accessibilityIdentifier("backupResult")
        }
    }

    private var destinationSection: some View {
        VStack(alignment: .leading, spacing: 8) {
            Text("Destination").font(.headline)
            if let error = model.destinationError {
                Text("Destination setting unavailable: \(error.localizedDescription)").foregroundStyle(.red)
            }
            TextField("Backup destination", text: $model.destinationDraft)
                .disabled(!model.canSaveDestination)
                .accessibilityIdentifier("backupDestination")
            Text("Empty uses the default backups folder inside the data root.")
                .font(.footnote).foregroundStyle(.secondary)
            HStack(spacing: 12) {
                Button("Choose Folder…", action: chooseFolder)
                    .disabled(!model.canSaveDestination)
                    .accessibilityIdentifier("backupChooseFolder")
                Button("Apply") { model.saveDestination() }
                    .disabled(!model.canSaveDestination)
                    .accessibilityIdentifier("backupApply")
                Button("Use Default") {
                    model.destinationDraft = ""
                    model.saveDestination()
                }
                .disabled(!model.canSaveDestination)
                .accessibilityIdentifier("backupDefault")
            }
        }
    }

    private var actionRow: some View {
        HStack(spacing: 12) {
            Button("Back Up Now") { model.backUpNow() }
                .disabled(!model.canWrite)
                .accessibilityIdentifier("backupNow")
            Button("Refresh") { Task { await model.refresh() } }
                .disabled(model.isClosed)
                .accessibilityIdentifier("backupRefresh")
            if model.isRefreshing {
                ProgressView().controlSize(.small)
            }
        }
    }

    @ViewBuilder private var generationsSection: some View {
        Text("Generations").font(.headline)
        if case .failed(let error) = model.generationsReadState {
            Text(
                "Generation listing unavailable: \(error.localizedDescription). Any shown rows are from the last successful read."
            )
            .foregroundStyle(.red)
        } else if case .unloaded = model.generationsReadState {
            Text("The generation list has not been loaded.").foregroundStyle(.secondary)
        }
        if model.generations.isEmpty {
            if case .loaded = model.generationsReadState {
                Text("No backup generations recorded.").foregroundStyle(.secondary)
            }
        } else {
            List(selection: selection) {
                ForEach(model.generations) { generation in
                    row(generation)
                        .tag(generation.generationId)
                        .accessibilityIdentifier("backupGeneration-\(generation.generationId)")
                }
            }
            .frame(minHeight: 160, maxHeight: 240)
        }
        Button("Restore Selected…") { model.requestRestore() }
            .disabled(!model.canRestore || restoreBlockedReason != nil)
            .accessibilityIdentifier("backupRestore")
        if let restoreBlockedReason {
            Text(restoreBlockedReason).foregroundStyle(.secondary)
        }
    }

    /// Only a complete row can become the restore selection; an old
    /// verdict never gates it, the restore verifies again.
    private var selection: Binding<String?> {
        Binding(
            get: { model.selectedGenerationId },
            set: { id in
                model.selectedGenerationId =
                    model.generations.contains { $0.id == id && $0.state == .complete } ? id : nil
            })
    }

    private func row(_ generation: BackupGeneration) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(BackupsPresentation.state(generation.state))
                Text(generation.generationId)
                    .foregroundStyle(.secondary)
                    .lineLimit(1).truncationMode(.middle)
            }
            Text(
                "Started \(BackupsPresentation.date(generation.startedAtMs)); "
                    + "completed \(BackupsPresentation.date(generation.completedAtMs))"
            ).font(.caption)
            Text(
                "\(BackupsPresentation.byteCount(generation.totalBytes)); "
                    + "\(BackupsPresentation.objectCount(generation.objectCount)); "
                    + BackupsPresentation.verification(generation.verification)
            ).font(.caption)
            HStack {
                Text(generation.location).textSelection(.enabled)
                Button("Copy location") { copy(generation.location) }
                    .buttonStyle(.borderless)
                    .accessibilityIdentifier("backupLocationCopy-\(generation.id)")
            }.font(.caption)
        }
        .padding(.vertical, 2)
    }

    @ViewBuilder private var artifactsSection: some View {
        Text("Retained restore data").font(.headline)
        Text(
            "These names do not prove ownership, type or readiness to restore. Preserved "
                + "roots can hold the listed generations and a stage may still be in use. "
                + "Remove them only by deliberate manual filesystem deletion."
        ).font(.footnote).foregroundStyle(.secondary)
        if case .failed(let error) = model.artifactsReadState {
            Text(
                "Retained paths unavailable: \(error.localizedDescription). Any shown names are from the last successful read."
            )
            .foregroundStyle(.red)
        } else if case .unloaded = model.artifactsReadState {
            Text("Retained paths have not been loaded.").foregroundStyle(.secondary)
        }
        if model.artifacts.isEmpty {
            if case .loaded = model.artifactsReadState {
                Text("None retained.").foregroundStyle(.secondary)
            }
        } else {
            ForEach(model.artifacts) { artifact in
                HStack(alignment: .firstTextBaseline, spacing: 8) {
                    Text(BackupsPresentation.artifactKind(artifact.kind))
                    Text(artifact.path).textSelection(.enabled)
                    Button("Copy path") { copy(artifact.path) }
                        .accessibilityIdentifier("backupArtifactCopy-\(artifact.path)")
                }
                .accessibilityIdentifier("backupArtifact-\(artifact.path)")
            }
        }
    }

    private var confirmationPresented: Binding<Bool> {
        Binding(
            get: { model.confirmation != nil },
            set: { shown in
                if !shown {
                    confirmError = nil
                    model.dismissConfirmation()
                }
            })
    }

    @ViewBuilder private var confirmationSheet: some View {
        if let chosen = model.confirmation {
            VStack(alignment: .leading, spacing: 16) {
                Text("Restore this backup?").font(.headline)
                Text(BackupsPresentation.confirmation(chosen))
                if let confirmError {
                    Text(confirmError).foregroundStyle(.red)
                }
                HStack {
                    Spacer()
                    Button("Cancel") {
                        confirmError = nil
                        model.dismissConfirmation()
                    }
                    .keyboardShortcut(.cancelAction)
                    .accessibilityIdentifier("backupDismissRestore")
                    Button("Restore", role: .destructive) {
                        confirmError =
                            confirmRestore() ? nil : "The restore could not start now."
                    }
                    .accessibilityIdentifier("backupConfirmRestore")
                }
            }
            .padding(24)
            .frame(width: 440)
        }
    }

    /// An existing-folder choice only; the exact path is never checked
    /// or second-guessed by the view.
    private func copy(_ path: String) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(path, forType: .string)
    }

    private func chooseFolder() {
        guard model.canSaveDestination else { return }
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.canCreateDirectories = false
        panel.allowsMultipleSelection = false
        panel.prompt = "Choose"
        panel.message = "Choose an existing folder for backups."
        guard panel.runModal() == .OK, let url = panel.url else { return }
        model.destinationDraft = url.path(percentEncoded: false)
    }
}
