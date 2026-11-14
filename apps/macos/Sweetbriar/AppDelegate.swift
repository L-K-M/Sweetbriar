// The app shell (spec/08 "Windows") plus the pilot's Debug menu
// (spec/08 "Debug menu"): the main window — sidebar, message list,
// reader — over the local archive, and a code-built main menu carrying
// the synthetic fill, the list prototype and the implementation
// toggle. The core opens off the main thread in a `CoreSession`; a
// failure shows in the main window.

import AppKit
import SweetbriarBridge

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private var mainWindow: MainWindowController?
    private var session: CoreSession?
    private var settingsWindow: AppearanceSettingsWindow?
    private let appearance = SharedAppearance()
    private var backups: SharedBackups?
    private var composeWindows: ComposeWindows?
    private var composeCues: SharedComposeCue?
    private var terminating = false

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

        // The bridge batches setting snapshots on the display clock.
        let clockView = NSView()
        let batcher = FrameBatcher<[Setting]>(clock: DisplayLinkClock(view: clockView)) { _ in }

        // The session opens off the main thread at once; quitting at
        // any point later — mid-open included — shuts down what it
        // opened. `-dataRoot <path> -cacheRoot <path>` opens other
        // roots: UI tests import into fresh ones (spec/11).
        let roots = LaunchRoots.fromArguments()
        let session = CoreSession {
            if let roots {
                return try await CoreBridge.open(config: roots, snapshots: batcher)
            }
            return try await CoreBridge.open(snapshots: batcher)
        }
        self.session = session

        let backups = SharedBackups(
            beforeRestore: { [weak self] in
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
        composeCues = SharedComposeCue(canWrite: { [weak backups] in backups?.canWrite == true })
        composeWindows = ComposeWindows(backups: backups, openCore: { try await session.bridge() },
            reportError: { [weak self] error in self?.presentError("Couldn't create a draft", error) })
        let mainWindow = MainWindowController(session: session, appearance: appearance, backups: backups,
                                              openBackups: { [weak self] in self?.showSettings(tab: .backups) })
        self.mainWindow = mainWindow
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
            onAddBridgeAccount: { [weak self] in
                self?.mainWindow?.showWindow(nil)
                self?.mainWindow?.beginBridgeAccountSetup()
            },
            onSettings: { [weak self] in self?.showSettings() },
            isWriteEnabled: { [weak self] in self?.backups?.canWrite == true },
            onNewMessage: { [weak self] in
                guard let self, !terminating else { return }
                composeWindows?.newMessage(preferredAccountId: mainWindow?.sidebar?.selectedAccountId)
            },
            isComposeEnabled: { [weak self] in self?.backups?.canCompose == true }
        )
        NSApplication.shared.mainMenu = builder.makeMainMenu()
        menuBuilder = builder
    }

    private func showSettings(tab: SettingsTab? = nil) {
        guard !terminating, let session, let backups else { return }
        if settingsWindow == nil {
            settingsWindow = AppearanceSettingsWindow(session: session, shared: appearance, backups: backups)
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
        await composeCues?.reset()
        await prototypeController?.closeAndAwaitModel()
        prototypeController = nil
        await mainWindow?.retireContent()
        await appearance.reset()
        if !terminating { await mainWindow?.rebuildAfterRestore() }
    }

    private func archiveClosed() async {
        settingsWindow?.retireEditor()
        await composeCues?.stopAndWait()
        await prototypeController?.closeAndAwaitModel()
        prototypeController = nil
        await mainWindow?.archiveClosed()
        await appearance.reset()
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
            await backups?.cancelAndWait()
            await settingsWindow?.cancelAndWait()
            await composeCues?.stopAndWait()
            await prototypeController?.closeAndAwaitModel()
            await mainWindow?.retireContent()
            await appearance.reset()
            await session.shutdown()
            NSApplication.shared.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }
}
