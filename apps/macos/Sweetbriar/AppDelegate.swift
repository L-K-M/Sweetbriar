// The M0.9 shell (spec/08 "Windows"): one main window showing the
// core's `appearance.compose_cue` setting, refreshed at most once per
// display frame through `FrameBatcher`. The core opens off the main
// thread; an open failure lands in the label instead.

import AppKit
import SweetbriarBridge

@MainActor
final class AppDelegate: NSObject, NSApplicationDelegate {
    private static let composeCueKey = "appearance.compose_cue"

    private var window: NSWindow?
    private var bridge: CoreBridge?
    private let cueLabel = NSTextField(labelWithString: "Opening the core…")

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

        Task.detached { [weak self] in
            do {
                let bridge = try await CoreBridge.open(snapshots: batcher)
                _ = try await bridge.settings()
                await MainActor.run {
                    self?.bridge = bridge
                }
            } catch {
                await MainActor.run {
                    self?.cueLabel.stringValue =
                        "Open failed: \(error.localizedDescription)"
                }
            }
        }
    }

    /// spec/07 "Lifecycle": the app can't die while the core is
    /// closing — defer, await `shutdown()` (uncancellable, waits for
    /// calls in flight), then let the quit proceed.
    func applicationShouldTerminate(_ sender: NSApplication)
        -> NSApplication.TerminateReply
    {
        guard let bridge else {
            return .terminateNow
        }
        Task { @MainActor in
            await bridge.shutdown()
            NSApplication.shared.reply(toApplicationShouldTerminate: true)
        }
        return .terminateLater
    }
}
