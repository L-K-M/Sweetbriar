// The app shell (spec/08 "Windows") plus the pilot's Debug menu
// (spec/08 "Debug menu"): the main window — sidebar, message list,
// reader — over the local archive, and a code-built main menu carrying
// the synthetic fill, the list prototype and the implementation
// toggle. The core opens off the main thread in a `CoreSession`; a
// failure shows in the main window.

import AppKit
import UserNotifications
import SweetbriarBridge

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate, UNUserNotificationCenterDelegate {
    private var mainWindow: MainWindowController?
    private var session: CoreSession?
    private var settingsWindow: AppearanceSettingsWindow?
    private var activityWindow: ActivityWindowController?
    private var shortcutHelpWindow: ShortcutHelpWindowController?
    private let appearance = SharedAppearance()
    private let readingViewMemory = SharedReadingViewMemory()
    private var backups: SharedBackups?
    private var composeWindows: ComposeWindows?
    private var composeCues: SharedComposeCue?
    /// The new-mail notification loop, started once the session's core
    /// is open and stopped with the archive or at termination
    /// (spec/10 "Notifications").
    private var newMailNotifier: NewMailNotifier?
    private var terminating = false
    /// Set once the archive closed; no new-mail notifier starts after.
    private var archiveIsClosed = false

    /// The menu builder is retained: `NSMenuItem.target` is weak, so
    /// the Debug items die with their builder otherwise.
    private var menuBuilder: MainMenuBuilder?

    /// The prototype window is a singleton — "List Prototype" brings
    /// the same one forward.
    private var prototypeController: ListPrototypeWindowController?

    /// The Composer Probe is a singleton the same way — Debug →
    /// Composer Probe brings the same one forward.
    private var composerProbeController: ComposerProbeWindowController?

    /// The running benchmark, or nil — the menu item greys out while
    /// one is in flight.
    private var benchmarkRunner: ListBenchmarkRunner?

    /// The report window the menu run shows, kept so it survives its
    /// own close.
    private var benchmarkReportController: BenchmarkReportWindowController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        installMainMenu()
        // A delivered notification's tap routes to its account's inbox;
        // the delegate is owned by the notification center for the
        // app's life (spec/10 "Notifications").
        UNUserNotificationCenter.current().delegate = self

        // The bridge batches setting snapshots on the display clock.
        let clockView = NSView()
        let batcher = FrameBatcher<[Setting]>(clock: DisplayLinkClock(view: clockView)) { _ in }

        // The session opens off the main thread at once; quitting at
        // any point later — mid-open included — shuts down what it
        // opened. `-dataRoot <path> -cacheRoot <path>` opens other
        // roots: UI tests import into fresh ones (spec/11).
        let roots = LaunchRoots.fromArguments()
        // External edits a crash left behind go unread (spec/10).
        if let cacheRoot = (roots ?? (try? CoreBridge.platformDefaultConfig()))?.cacheRoot {
            let cache = URL(fileURLWithPath: cacheRoot, isDirectory: true)
            ExternalEditFiles.purge(ExternalEditFiles.root(inCache: cache))
            // The opt-in latency log lives beside them (spec/08).
            LatencyRecorder.shared.open(cacheRoot: cache)
        }
        let session = CoreSession {
            if let roots {
                return try await CoreBridge.open(config: roots, snapshots: batcher)
            }
            return try await CoreBridge.open(snapshots: batcher)
        }
        self.session = session
        observeAppActive()

        // The notifier starts once the archive session is open, never
        // at launch: a permission request waits for the first arrival.
        Task { @MainActor [weak self] in
            guard let self, !self.terminating else { return }
            let bridge = try? await session.bridge()
            // Re-check after the open: a quit or an archive close that
            // ran meanwhile already stopped the (then absent) notifier,
            // so starting one now would outlive the archive. A failed
            // open surfaces in the main window; there is nothing to
            // notify about without an archive.
            guard let bridge, !self.terminating, !self.archiveIsClosed else { return }
            let notifier = NewMailNotifier(
                source: bridge, poster: UNUserNotificationCenterPoster())
            self.newMailNotifier = notifier
            notifier.start()
        }

        let backups = SharedBackups(
            beforeRestore: { [weak self] in
                // Close the Activity window before the swap, so its
                // reads end with the old core.
                self?.retireActivityWindow()
                self?.composeCues?.freezeForRestore()
                self?.settingsWindow?.retireEditor()
                self?.mainWindow?.retireWritersForRestore()
            },
            prepareRestore: { [weak self] in await self?.composeCues?.waitForRetirement() },
            didKeepArchive: { [weak self] in await self?.composeCues?.reset() },
            didRestore: { [weak self] in await self?.archiveRestored() },
            didClose: { [weak self] in await self?.archiveClosed() }
        )
        self.backups = backups
        let composeCue = SharedComposeCue(canWrite: { [weak backups] in backups?.canWrite == true })
        composeCues = composeCue
        composeWindows = ComposeWindows(backups: backups, appearance: appearance, cues: composeCue,
            openCore: { try await session.bridge() },
            reportError: { [weak self] action, error in
                switch action {
                case .create: self?.presentError("Couldn't create a draft", error)
                case .open: self?.presentError("Couldn't open the draft", error)
                }
            })
        if let composeWindows { ComposeRestorer.shared.attach(composeWindows) }
        let mainWindow = MainWindowController(session: session, appearance: appearance, cues: composeCue,
                                              backups: backups,
                                              readingViewMemory: readingViewMemory,
                                              openBackups: { [weak self] in self?.showSettings(tab: .backups) },
                                              openDraft: { [weak self] in self?.composeWindows?.open(draftId: $0) },
                                              composeCommands: menuBuilder)
        composeWindows?.onWindowsChanged = { [weak mainWindow] in mainWindow?.draftsChanged() }
        self.mainWindow = mainWindow
        mainWindow.onOpenActivity = { [weak self] in self?.showActivity() }
        mainWindow.onComposeFromMessage = { [weak self] in self?.composeFromMessage($0) }
        mainWindow.showWindow(nil)
        mainWindow.start()

        // `-runListBenchmark <report.json>` (spec/08 "Debug menu"):
        // the same benchmark the menu runs, then the report written
        // to the path and the app quits — macOS CI drives it.
        if let reportPath = UserDefaults.standard.string(forKey: "runStoreBenchmark") {
            startBenchmark(mode: .store, reportPath: reportPath)
        } else if let reportPath = UserDefaults.standard.string(forKey: "runSearchBenchmark") {
            startBenchmark(mode: .search, reportPath: reportPath)
        } else if let reportPath = UserDefaults.standard.string(
            forKey: "runListBenchmark"
        ) {
            startBenchmark(reportPath: reportPath)
        }
    }

    /// Build and install the main menu — its own type owns the items
    /// (spec/08 "Debug menu (pilot)").
    private func installMainMenu() {
        let builder = MainMenuBuilder(
            onFillSynthetic: { [weak self] in
                self?.fillSyntheticRows()
            },
            onShowPrototype: { [weak self] in
                self?.showListPrototype()
            },
            onImplementationChanged: { [weak self] implementation in
                self?.prototypeImplementationChanged(implementation)
            },
            onRunBenchmark: { [weak self] in
                self?.startBenchmark()
            },
            onRunStoreBenchmark: { [weak self] in
                self?.startBenchmark(mode: .store)
            },
            onRunSearchBenchmark: { [weak self] in
                self?.startBenchmark(mode: .search)
            },
            isBenchmarkEnabled: { [weak self] in
                self?.benchmarkRunner == nil && self?.backups?.writesUnavailable != true
                    && self?.backups?.restoreBlockedReason == nil
            },
            onShowComposerProbe: { [weak self] in
                self?.showComposerProbe()
            },
            onImport: { [weak self] in
                self?.mainWindow?.showWindow(nil)
                self?.mainWindow?.beginImport()
            },
            onAddProtonAccount: { [weak self] in
                self?.mainWindow?.showWindow(nil)
                self?.mainWindow?.beginProtonSignIn()
            },
            onSettings: { [weak self] in self?.showSettings() },
            onShowActivity: { [weak self] in self?.showActivity() },
            onShowShortcuts: { [weak self] in self?.showShortcutHelp() },
            onShowViewer: { [weak self] in self?.mainWindow?.showWindow(nil) },
            isWriteEnabled: { [weak self] in self?.backups?.canWrite == true },
            onNewMessage: { [weak self] in
                guard let self, !terminating else { return }
                composeWindows?.newMessage(preferredAccountId: mainWindow?.sidebar?.selectedAccountId)
            },
            isComposeEnabled: { [weak self] in self?.backups?.canCompose == true },
            onReply: { [weak self] in
                guard let self, let occurrence = mainWindow?.replyOccurrenceId else { return }
                composeFromMessage(.reply(occurrenceId: occurrence))
            },
            onReplyAll: { [weak self] in
                guard let self, let occurrence = mainWindow?.replyOccurrenceId else { return }
                composeFromMessage(.replyAll(occurrenceId: occurrence))
            },
            onForward: { [weak self] in
                guard let self, let occurrence = mainWindow?.replyOccurrenceId else { return }
                composeFromMessage(.forward(occurrenceId: occurrence))
            },
            isReplyEnabled: { [weak self] in self?.mainWindow?.replyOccurrenceId != nil }
        )
        NSApplication.shared.mainMenu = builder.makeMainMenu()
        menuBuilder = builder
    }

    private func composeFromMessage(_ source: DraftSource) {
        guard !terminating else { return }
        composeWindows?.compose(source: source)
    }

    /// Window → Activity: bring the shared window forward, else make
    /// one over the main window's Activity model (spec/08).
    private func showActivity() {
        guard !terminating, let mainWindow,
              let activity = mainWindow.activity, let bridge = mainWindow.bridge,
              // A retired content leaves no model, and a model from an
              // older bridge must not open over the current one.
              mainWindow.activitySourceID == ObjectIdentifier(bridge)
        else { return }
        if activityWindow == nil {
            activityWindow = ActivityWindowController(
                model: activity, bridge: bridge, appearance: appearance.model(source: bridge)
            ) {
                [weak self] account in
                self?.mainWindow?.beginProtonSignIn(signingInAgain: account)
            }
        }
        activityWindow?.showWindow(nil)
        activityWindow?.window?.makeKeyAndOrderFront(nil)
    }

    /// Help → Keyboard Shortcuts…: one shared reference window, made
    /// on first use and brought forward after that.
    private func showShortcutHelp() {
        guard !terminating else { return }
        if shortcutHelpWindow == nil {
            shortcutHelpWindow = ShortcutHelpWindowController()
        }
        shortcutHelpWindow?.showWindow(nil)
        shortcutHelpWindow?.window?.makeKeyAndOrderFront(nil)
    }

    /// The Activity window holds the old core's model and bridge, so a
    /// restore or close drops it; Window → Activity builds a new one.
    private func retireActivityWindow() {
        activityWindow?.close()
        activityWindow = nil
    }

    /// spec/07 "Lifecycle": report the app's active state to the core's
    /// workers. A failure is logged, never shown.
    private func observeAppActive() {
        let center = NotificationCenter.default
        center.addObserver(
            self, selector: #selector(applicationDidBecomeActive),
            name: NSApplication.didBecomeActiveNotification, object: NSApplication.shared)
        center.addObserver(
            self, selector: #selector(applicationDidResignActive),
            name: NSApplication.didResignActiveNotification, object: NSApplication.shared)
        Task { @MainActor in
            await self.reportAppActive(NSApplication.shared.isActive)
        }
    }

    @objc private func applicationDidBecomeActive() {
        Task { @MainActor in await self.reportAppActive(true) }
    }

    @objc private func applicationDidResignActive() {
        Task { @MainActor in await self.reportAppActive(false) }
    }

    private func reportAppActive(_ active: Bool) async {
        guard let session else { return }
        do {
            let bridge = try await session.bridge()
            try await bridge.setAppActive(active)
        } catch {
            print("set_app_active(\(active)): \(error.localizedDescription)")
        }
    }

    private func showSettings(tab: SettingsTab? = nil) {
        guard !terminating, let session, let backups, let composeCues else { return }
        if settingsWindow == nil {
            settingsWindow = AppearanceSettingsWindow(
                session: session, shared: appearance, cues: composeCues, backups: backups,
                onSignInAgain: { [weak self] account in
                    self?.mainWindow?.beginProtonSignIn(signingInAgain: account)
                })
        }
        if let tab { settingsWindow?.select(tab) }
        settingsWindow?.showWindow(nil)
        settingsWindow?.window?.makeKeyAndOrderFront(nil)
    }

    /// Debug → Fill 500,000 Synthetic Rows: fill through the session's
    /// bridge, then hand the new generation to every open prototype
    /// model so it rebuilds — the prototype has no subscriptions
    /// (spec/08). Failures show in an alert.
    private func fillSyntheticRows() {
        guard let backups, backups.begin(.syntheticFill) else {
            presentError("The synthetic fill is unavailable", CoreFailure.conflict(detail: "Finish the current operation or recover the read-only archive first."))
            return
        }
        Task { @MainActor [weak self] in
            defer { backups.end(.syntheticFill) }
            guard let self, let session else { return }
            do {
                let bridge = try await session.bridge()
                _ = try await bridge.debugFillSynthetic(
                    count: MainMenuBuilder.syntheticFillCount
                )
                try await prototypeController?.modelDidRefill()
            } catch {
                presentError("The synthetic fill failed", error)
            }
        }
    }

    /// Debug → List Prototype: bring the open window forward, else
    /// make one listing `DebugSynthetic` in the stored
    /// implementation.
    private func showListPrototype() {
        guard !terminating, backups?.isRestoring != true else { return }
        if let controller = prototypeController {
            controller.window?.makeKeyAndOrderFront(nil)
            return
        }
        guard let session else { return }
        let controller = ListPrototypeWindowController(
            session: session,
            implementation: .stored
        )
        controller.onWindowClosed = { [weak self] in
            self?.prototypeController = nil
        }
        prototypeController = controller
        controller.showWindow(nil)
        controller.openList()
    }

    /// Debug → Composer Probe (spec/08, plan M0.11): the composer's
    /// editor without the core. Bring the open one forward, else make
    /// one.
    private func showComposerProbe() {
        if let controller = composerProbeController {
            controller.window?.makeKeyAndOrderFront(nil)
            return
        }
        let controller = ComposerProbeWindowController()
        controller.onWindowClosed = { [weak self] in
            self?.composerProbeController = nil
        }
        composerProbeController = controller
        controller.showWindow(nil)
    }

    /// List Implementation ▸: the builder already persisted the pick;
    /// swap the open window's list if there is one.
    private func prototypeImplementationChanged(
        _ implementation: PrototypeImplementation
    ) {
        guard backups?.isRestoring != true, let controller = prototypeController else { return }
        Task { @MainActor in
            await controller.setImplementation(implementation)
        }
    }

    /// Debug → Run List Benchmark (spec/08): the four ABBA runs in
    /// fresh prototype windows, then the report in a window. With a
    /// `reportPath` — the launch-argument run — the report is written
    /// there, a short summary prints to stdout, and the app quits.
    private func startBenchmark(
        mode: BenchmarkMode = .synthetic, reportPath: String? = nil
    ) {
        guard let session, benchmarkRunner == nil else { return }
        let admitted = backups?.begin(.benchmark) == true
        let runner = ListBenchmarkRunner(session: session)
        benchmarkRunner = runner
        Task { @MainActor [weak self] in
            // An open List Prototype — a SwiftUI one can hold gigabytes
            // — would sit in every run's footprint: close it first.
            if let prototype = self?.prototypeController {
                await prototype.closeAndAwaitModel()
                self?.prototypeController = nil
            }
            let report: ListBenchmarkReport
            if admitted {
                switch mode {
                case .synthetic:
                    report = await runner.run()
                case .store:
                    report = await runner.runStore(controller: self?.mainWindow)
                case .search:
                    report = await runner.runSearch(controller: self?.mainWindow)
                }
            } else {
                report = ListBenchmarkReport(
                    ok: false, failures: ["The archive is unavailable or another archive operation is active."],
                    environment: BenchmarkEnvironment.collect(), runs: [], comparison: nil, mode: mode
                )
            }
            guard let self else { return }
            if admitted { backups?.end(.benchmark) }
            benchmarkRunner = nil
            guard let reportPath else {
                let controller = BenchmarkReportWindowController(
                    report: report
                )
                benchmarkReportController = controller
                controller.showWindow(nil)
                return
            }
            // "a run that can't start still writes ok: false" — the
            // runner's report already carries every failure; only the
            // write can still fail.
            let data = (try? report.encoded())
                ?? Data(#"{ "ok": false, "failures": ["the report failed to encode"] }"#
                    .utf8)
            do {
                try data.write(
                    to: URL(fileURLWithPath: reportPath),
                    options: .atomic
                )
            } catch {
                print(
                    "list-benchmark: couldn't write \(reportPath): "
                        + error.localizedDescription
                )
            }
            // Unbuffered: stdout is a file under `open --stdout`.
            FileHandle.standardOutput.write(
                Data((report.stdoutSummary + "\n").utf8)
            )
            // Not from this task: it runs as a main-queue job, and the
            // run loop `.terminateLater` spins doesn't drain the main
            // queue inside one — the shutdown task would never run.
            // A run-loop block runs outside the queue, on the main
            // thread.
            RunLoop.main.perform {
                MainActor.assumeIsolated {
                    NSApp.terminate(nil)
                }
            }
        }
    }

    /// All old-context handles must finish before any replacement pane opens.
    private func archiveRestored() async {
        settingsWindow?.retireEditor()
        retireActivityWindow()
        await composeCues?.reset()
        await prototypeController?.closeAndAwaitModel()
        prototypeController = nil
        await mainWindow?.retireContent()
        await appearance.reset()
        readingViewMemory.reset()
        if !terminating { await mainWindow?.rebuildAfterRestore() }
    }

    private func archiveClosed() async {
        archiveIsClosed = true
        await newMailNotifier?.stop()
        newMailNotifier = nil
        settingsWindow?.retireEditor()
        retireActivityWindow()
        await composeCues?.stopAndWait()
        await prototypeController?.closeAndAwaitModel()
        prototypeController = nil
        await mainWindow?.archiveClosed()
        await appearance.reset()
        readingViewMemory.reset()
    }

    /// Debug failures surface as an alert — a sheet when a window can
    /// host it, modal otherwise.
    private func presentError(_ title: String, _ error: any Error) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = error.localizedDescription
        alert.alertStyle = .warning
        if let keyWindow = NSApplication.shared.keyWindow {
            alert.beginSheetModal(for: keyWindow, completionHandler: nil)
        } else {
            _ = alert.runModal()
        }
    }

    /// Explicit roots from the launch arguments, both or neither.
    private enum LaunchRoots {
        static let dataRootKey = "dataRoot"
        static let cacheRootKey = "cacheRoot"

        static func fromArguments() -> CoreConfig? {
            let defaults = UserDefaults.standard
            guard let data = defaults.string(forKey: dataRootKey),
                  let cache = defaults.string(forKey: cacheRootKey)
            else {
                return nil
            }
            return CoreConfig(dataRoot: data, cacheRoot: cache)
        }
    }

    /// The Dock icon brings the main window back after it was closed.
    func applicationShouldHandleReopen(
        _ sender: NSApplication,
        hasVisibleWindows flag: Bool
    ) -> Bool {
        if !flag {
            mainWindow?.showWindow(nil)
        }
        return true
    }

    /// spec/07 "Lifecycle": the app can't die while the core is
    /// closing — defer, await `shutdown()` (uncancellable, waits for
    /// calls in flight), then let the quit proceed.
    /// Compose windows restore by draft id only (spec/08 "Windows").
    func applicationSupportsSecureRestorableState(_ app: NSApplication) -> Bool { true }

    func applicationShouldTerminate(_ sender: NSApplication)
        -> NSApplication.TerminateReply
    {
        guard let session else {
            return .terminateNow
        }
        guard !terminating else { return .terminateLater }
        terminating = true
        Task { @MainActor in
            guard await composeWindows?.closeForQuit() != false else {
                terminating = false
                NSApplication.shared.reply(toApplicationShouldTerminate: false)
                return
            }
            composeCues?.stop()
            settingsWindow?.close()
            activityWindow?.close()
            await backups?.cancelAndWait()
            await settingsWindow?.cancelAndWait()
            await composeCues?.stopAndWait()
            await prototypeController?.closeAndAwaitModel()
            await newMailNotifier?.stop()
            await mainWindow?.retireContent()
            await appearance.reset()
            await session.shutdown()
            NSApplication.shared.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }

    /// Show new-mail banners while Sweetbriar is frontmost too, so a
    /// tap can always reach the inbox it names. These are the
    /// completion-handler `UNUserNotificationCenterDelegate` methods;
    /// the Objective-C runtime only exposes those selectors, and they
    /// arrive off the main actor.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        willPresent notification: UNNotification,
        withCompletionHandler completionHandler: @escaping (UNNotificationPresentationOptions) -> Void
    ) {
        completionHandler([.banner, .list, .sound])
    }

    /// A tapped banner opens the account it names. Other actions (none
    /// ship yet) and background deliveries change nothing here.
    nonisolated func userNotificationCenter(
        _ center: UNUserNotificationCenter,
        didReceive response: UNNotificationResponse,
        withCompletionHandler completionHandler: @escaping () -> Void
    ) {
        guard response.actionIdentifier == UNNotificationDefaultActionIdentifier,
              let accountId = NewMailNotificationTarget.accountID(
                from: response.notification.request.content.userInfo)
        else {
            completionHandler()
            return
        }
        Task { @MainActor in
            NSApp.activate(ignoringOtherApps: true)
            self.mainWindow?.showWindow(nil)
            self.mainWindow?.revealAccountInbox(accountId: accountId)
        }
        completionHandler()
    }
}
