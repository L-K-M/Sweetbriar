// The M0.9 shell (spec/08 "Windows") plus the pilot's Debug menu
// (spec/08 "Debug menu"): one main window showing the core's
// `appearance.compose_cue` setting, refreshed at most once per display
// frame through `FrameBatcher`; a code-built main menu carrying the
// synthetic fill, the list prototype and the implementation toggle.
// The core opens off the main thread in a `CoreSession`; a failure
// lands in the label.

import AppKit
import SweetbriarBridge

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private static let composeCueKey = "appearance.compose_cue"

    private var window: NSWindow?
    private var session: CoreSession?
    private let cueLabel = NSTextField(labelWithString: "Opening the core…")

    /// The menu builder is retained: `NSMenuItem.target` is weak, so
    /// the Debug items die with their builder otherwise.
    private var menuBuilder: MainMenuBuilder?

    /// The prototype window is a singleton — "List Prototype" brings
    /// the same one forward.
    private var prototypeController: ListPrototypeWindowController?

    /// The running benchmark, or nil — the menu item greys out while
    /// one is in flight.
    private var benchmarkRunner: ListBenchmarkRunner?

    /// The report window the menu run shows, kept so it survives its
    /// own close.
    private var benchmarkReportController: BenchmarkReportWindowController?

    func applicationDidFinishLaunching(_ notification: Notification) {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 440, height: 140),
            styleMask: [.titled, .closable, .miniaturizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Sweetbriar"
        cueLabel.alignment = .center
        cueLabel.translatesAutoresizingMaskIntoConstraints = false
        let content = NSView()
        content.addSubview(cueLabel)
        NSLayoutConstraint.activate([
            cueLabel.centerXAnchor.constraint(equalTo: content.centerXAnchor),
            cueLabel.centerYAnchor.constraint(equalTo: content.centerYAnchor),
        ])
        window.contentView = content
        window.center()
        window.makeKeyAndOrderFront(nil)
        self.window = window

        installMainMenu()

        let batcher = FrameBatcher<[Setting]>(clock: DisplayLinkClock(view: content)) {
            [weak self] batch in
            // A batch is several snapshots; only the last matters —
            // it's the newest core state.
            guard let snapshot = batch.last,
                let cue = snapshot.first(where: { $0.key == Self.composeCueKey })
            else {
                return
            }
            self?.cueLabel.stringValue = "Compose cue: \(cue.value)"
        }

        // The session opens off the main thread at once; quitting at
        // any point later — mid-open included — shuts down what it
        // opened.
        let session = CoreSession {
            try await CoreBridge.open(snapshots: batcher)
        }
        self.session = session
        Task { [weak self] in
            do {
                _ = try await session.bridge().settings()
            } catch {
                self?.cueLabel.stringValue =
                    "Couldn't load the setting: \(error.localizedDescription)"
            }
        }

        // `-runListBenchmark <report.json>` (spec/08 "Debug menu"):
        // the same benchmark the menu runs, then the report written
        // to the path and the app quits — macOS CI drives it.
        if let reportPath = UserDefaults.standard.string(
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
            isBenchmarkEnabled: { [weak self] in
                self?.benchmarkRunner == nil
            }
        )
        NSApplication.shared.mainMenu = builder.makeMainMenu()
        menuBuilder = builder
    }

    /// Debug → Fill 500,000 Synthetic Rows: fill through the session's
    /// bridge, then hand the new generation to every open prototype
    /// model so it rebuilds — the prototype has no subscriptions
    /// (spec/08). Failures show in an alert.
    private func fillSyntheticRows() {
        Task { @MainActor [weak self] in
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

    /// List Implementation ▸: the builder already persisted the pick;
    /// swap the open window's list if there is one.
    private func prototypeImplementationChanged(
        _ implementation: PrototypeImplementation
    ) {
        guard let controller = prototypeController else { return }
        Task { @MainActor in
            await controller.setImplementation(implementation)
        }
    }

    /// Debug → Run List Benchmark (spec/08): the four ABBA runs in
    /// fresh prototype windows, then the report in a window. With a
    /// `reportPath` — the launch-argument run — the report is written
    /// there, a short summary prints to stdout, and the app quits.
    private func startBenchmark(reportPath: String? = nil) {
        guard let session, benchmarkRunner == nil else { return }
        let runner = ListBenchmarkRunner(session: session)
        benchmarkRunner = runner
        Task { @MainActor [weak self] in
            // An open List Prototype — a SwiftUI one can hold gigabytes
            // — would sit in every run's footprint: close it first.
            if let prototype = self?.prototypeController {
                await prototype.closeAndAwaitModel()
                self?.prototypeController = nil
            }
            let report = await runner.run()
            guard let self else { return }
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

    /// spec/07 "Lifecycle": the app can't die while the core is
    /// closing — defer, await `shutdown()` (uncancellable, waits for
    /// calls in flight), then let the quit proceed.
    func applicationShouldTerminate(_ sender: NSApplication)
        -> NSApplication.TerminateReply
    {
        guard let session else {
            return .terminateNow
        }
        Task { @MainActor in
            await session.shutdown()
            NSApplication.shared.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }
}
