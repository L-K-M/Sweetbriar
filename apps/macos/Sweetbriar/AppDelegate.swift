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
