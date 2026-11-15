import AppKit
import SQLite3
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

private struct RestoreAppearanceClock: FrameClock {
    func nextFrame() async { await Task.yield() }
}

@MainActor
private func restoreAppearanceEventually(_ check: @MainActor () async -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(15)
    while !(await check()) {
        if Task.isCancelled || ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(10))
    }
    return true
}

/// A paused synthetic account has no endpoint or credentials and cannot make
/// network calls. Seed only after Core has closed its store, as the Rust
/// service compose fixture does; all subsequent operations use CoreBridge.
private func seedRestoreAppearanceIdentity(_ path: String) throws {
    var handle: OpaquePointer?
    let opened = sqlite3_open_v2(path, &handle, SQLITE_OPEN_READWRITE, nil)
    defer { if let handle { sqlite3_close(handle) } }
    try #require(opened == SQLITE_OK)
    let db = try #require(handle)
    let sql = """
        BEGIN;
        INSERT INTO accounts(account_id,kind,provider,display_name,state,sort_order,created_at)
          VALUES('restore-account','imap','generic_imap','Work','paused',2,0);
        INSERT INTO account_appearance(account_id,color_srgb,assignment,revision)
          VALUES('restore-account','#0072b2','custom',1);
        UPDATE store_meta SET value = CAST(CAST(value AS INTEGER) + 1 AS TEXT)
          WHERE key = 'appearance_generation';
        INSERT INTO identities(identity_id,account_id,email,display_name,is_default,sendable,revision)
          VALUES('restore-identity','restore-account','alex@example.test','Alex Example',1,'validated',1);
        COMMIT;
        """
    var error: UnsafeMutablePointer<CChar>?
    let result = sqlite3_exec(db, sql, nil, nil, &error)
    let message = error.map { String(cString: $0) } ?? "SQLite seed failed"
    sqlite3_free(error)
    try #require(result == SQLITE_OK, "\(message)")
}

@MainActor
private final class RestoreAppearanceContext {
    var cues: SharedComposeCue?
    var appearance: SharedAppearance?
    var errors: [any Error] = []
}

@Suite(.serialized, .timeLimit(.minutes(2))) @MainActor
struct ComposeRestoredAppearanceTests {
    @Test func restoredDraftsRecoverIdentityAppearanceInBothWindows() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(
            "sb-compose-restore-\(UUID())")
        let config = CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path)
        let first = try await CoreBridge.open(
            config: config, snapshots: FrameBatcher<[Setting]>(clock: RestoreAppearanceClock()) { _ in })
        await first.shutdown()
        try seedRestoreAppearanceIdentity(root.appendingPathComponent("data/state.sqlite").path)
        let core = try await CoreBridge.open(
            config: config, snapshots: FrameBatcher<[Setting]>(clock: RestoreAppearanceClock()) { _ in })
        let context = RestoreAppearanceContext()
        let appearance = SharedAppearance()
        context.appearance = appearance
        let backups = SharedBackups(
            beforeRestore: { context.cues?.freezeForRestore() },
            prepareRestore: { await context.cues?.waitForRetirement() },
            didKeepArchive: { await context.cues?.reset() },
            didRestore: {
                await context.cues?.reset()
                await context.appearance?.reset()
            },
            didClose: { await context.cues?.stopAndWait() })
        let cues = SharedComposeCue(canWrite: { [weak backups] in backups?.canWrite == true })
        context.cues = cues
        let owner = ComposeWindows(
            backups: backups, appearance: appearance, cues: cues, openCore: { core },
            reportError: { context.errors.append($0) })
        var reopened: [ComposeWindowController] = []
        let result: Result<Void, any Error>
        do {
            let backup = try #require(backups.model(source: core))
            await backup.refresh()
            try #require(await restoreAppearanceEventually { backups.canWrite })
            var account = try #require(
                try await core.appearance().accounts.first { $0.accountId == "restore-account" })
            _ = try await core.setAccountAppearance(
                accountId: account.accountId, expectedRevision: account.revision,
                change: AccountAppearanceChange(color: .paletteV1(slot: 0), symbol: nil))
            account = try #require(
                try await core.appearance().accounts.first { $0.accountId == "restore-account" })
            let savedTokens = try #require(
                try await core.appearance().identities.first {
                    $0.identityId == "restore-identity"
                }
            ).tokens
            let cue = try #require(cues.model(source: core))
            try #require(await restoreAppearanceEventually { cue.mode == .standard })
            try #require(await cue.setMode(.strong))
            owner.newMessage(preferredAccountId: "restore-account")
            owner.newMessage(preferredAccountId: "restore-account")
            try #require(
                await restoreAppearanceEventually {
                    owner.windows.count == 2
                        && owner.windows.values.allSatisfy {
                            $0.model.content?.identityId == "restore-identity"
                                && $0.identityStrip.strip.accessibilityLabel()?.contains("alex@example.test")
                                    == true
                                && $0.window?.styleMask.contains(.fullSizeContentView) == true
                        }
                })
            let original = Array(owner.windows.values).sorted { $0.model.draftId < $1.model.draftId }
            for (index, controller) in original.enumerated() {
                controller.subjectField.stringValue = "Restored subject \(index)"
                controller.textView.string = "Restored source \(index)"
                controller.textDidChange(
                    Notification(name: NSText.didChangeNotification, object: controller.textView))
                try #require(await controller.saveAndWait())
            }
            let ids = original.map { $0.model.draftId }
            let saved = original.map { $0.model.content }
            backup.backUpNow()
            try #require(await restoreAppearanceEventually { !backup.isBusy })
            guard case .backup(let generation, .complete) = backup.lastResult else {
                throw CoreFailure.internalError(detail: "Synthetic appearance backup did not complete.")
            }
            try #require(await cue.setMode(.standard))
            _ = try await core.setAccountAppearance(
                accountId: account.accountId, expectedRevision: account.revision,
                change: AccountAppearanceChange(color: .paletteV1(slot: 4), symbol: nil))
            let replacedTokens = try #require(
                try await core.appearance().identities.first {
                    $0.identityId == "restore-identity"
                }
            ).tokens
            try #require(replacedTokens != savedTokens)
            try #require(
                await restoreAppearanceEventually {
                    original.allSatisfy {
                        guard case .identity(_, _, let tokens) = $0.identityStrip.strip.content else {
                            return false
                        }
                        return tokens == replacedTokens
                            && $0.window?.styleMask.contains(.fullSizeContentView) == false
                    }
                })
            backup.selectedGenerationId = generation
            backup.requestRestore()
            try #require(backup.confirmation != nil && backups.restoreBlockedReason != nil)
            #expect(!backups.confirmRestore())
            try #require(await owner.closeForQuit())
            try #require(
                await restoreAppearanceEventually {
                    owner.windows.isEmpty && backups.restoreBlockedReason == nil
                })
            backup.requestRestore()
            try #require(backups.confirmRestore())
            try #require(await restoreAppearanceEventually { !backup.isBusy })
            guard case .restore(.restored) = backup.lastResult else {
                throw CoreFailure.internalError(detail: "Synthetic appearance restore did not complete.")
            }
            try #require(await restoreAppearanceEventually { backups.canWrite })
            let restoredCue = try #require(cues.model(source: core))
            let restoredAppearance = appearance.model(source: core)
            for id in ids {
                let controller = ComposeWindowController(
                    source: core, draftId: id, appearance: restoredAppearance, cue: restoredCue,
                    canWrite: { backups.canWrite })
                reopened.append(controller)
                controller.showWindow(nil)
            }
            try #require(
                await restoreAppearanceEventually {
                    restoredCue.mode == .strong
                        && reopened.allSatisfy {
                            $0.identityStrip.strip.accessibilityLabel()?.contains("alex@example.test") == true
                                && $0.window?.styleMask.contains(.fullSizeContentView) == true
                                && $0.model.content != nil
                        }
                })
            #expect(reopened.map { $0.model.content } == saved)
            for controller in reopened {
                guard case .identity(_, _, let tokens) = controller.identityStrip.strip.content else {
                    throw CoreFailure.internalError(detail: "Restored identity strip is unavailable.")
                }
                #expect(tokens == savedTokens)
                #expect(
                    controller.textView.drawsBackground
                        && controller.textView.backgroundColor == .textBackgroundColor)
            }
            let restoredText = reopened.map { $0.textView.string }
            for controller in reopened {
                controller.window?.makeKeyAndOrderFront(nil)
                controller.window?.makeFirstResponder(controller.textView)
                controller.textView.insertText(
                    "!", replacementRange: .init(location: controller.textView.string.utf16.count, length: 0))
                try #require(await controller.saveAndWait())
                try #require(controller.textView.undoManager?.canUndo == true)
            }
            let before = reopened.map { $0.model.content }
            let text = reopened.map { Array($0.textView.string.utf8) }
            let undo = reopened.map { $0.textView.undoManager?.canUndo }
            let redo = reopened.map { $0.textView.undoManager?.canRedo }
            try #require(await restoredCue.setMode(.standard))
            try #require(
                await restoreAppearanceEventually {
                    reopened.allSatisfy { $0.window?.styleMask.contains(.fullSizeContentView) == false }
                })
            let restoredAccount = try #require(
                try await core.appearance().accounts.first {
                    $0.accountId == "restore-account"
                })
            _ = try await core.setAccountAppearance(
                accountId: restoredAccount.accountId, expectedRevision: restoredAccount.revision,
                change: AccountAppearanceChange(color: .paletteV1(slot: 4), symbol: nil))
            let changedTokens = try #require(
                try await core.appearance().identities.first {
                    $0.identityId == "restore-identity"
                }
            ).tokens
            try #require(changedTokens != savedTokens)
            try #require(
                await restoreAppearanceEventually {
                    reopened.allSatisfy {
                        if case .identity(_, _, let tokens) = $0.identityStrip.strip.content {
                            return tokens == changedTokens
                        }
                        return false
                    }
                })
            #expect(reopened.map { $0.model.content } == before)
            #expect(reopened.map { Array($0.textView.string.utf8) } == text)
            #expect(reopened.map { $0.textView.undoManager?.canUndo } == undo)
            #expect(reopened.map { $0.textView.undoManager?.canRedo } == redo)
            for (index, controller) in reopened.enumerated() {
                let manager = try #require(controller.textView.undoManager)
                manager.undo()
                #expect(controller.textView.string == restoredText[index])
                try #require(manager.canRedo)
                manager.redo()
                #expect(Array(controller.textView.string.utf8) == text[index])
            }
            result = .success(())
        } catch { result = .failure(error) }
        for controller in reopened { #expect(await controller.closeWithoutSavingAndWait()) }
        if !(await owner.closeForQuit()) {
            for controller in Array(owner.windows.values) {
                #expect(await controller.closeWithoutSavingAndWait())
            }
        }
        await backups.cancelAndWait()
        await cues.stopAndWait()
        await appearance.reset()
        await core.shutdown()
        if case .success = result { try FileManager.default.removeItem(at: root) }
        try result.get()
    }
}
