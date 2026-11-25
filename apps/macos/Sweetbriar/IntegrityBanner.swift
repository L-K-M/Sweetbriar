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
        } else if let error = model.healthError {
            notice("Archive health unavailable", detail: error.localizedDescription)
                .padding(12)
        }
    }

    private func notice(_ title: String, detail: String) -> some View {
        HStack(alignment: .top, spacing: 12) {
            VStack(alignment: .leading, spacing: 4) {
                Text(title).font(.headline).accessibilityIdentifier("integrityBanner")
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

/// The banner under the main window's toolbar, across the window: a
/// titlebar accessory, so every pane's safe area makes room for it. It
/// takes the banner's height at the window's width and collapses while
/// the banner has nothing to say.
@MainActor
final class IntegrityBannerAccessory: NSTitlebarAccessoryViewController {
    private let model: BackupsModel
    private let banner: NSHostingController<IntegrityBanner>
    private var host: NSView { banner.view }

    init(model: BackupsModel, openBackups: @escaping @MainActor () -> Void) {
        self.model = model
        banner = NSHostingController(rootView: IntegrityBanner(model: model, openBackups: openBackups))
        // No size constraints from SwiftUI: an empty banner's would hold
        // the window to its own zero width. `measure` sizes the strip.
        banner.sizingOptions = []
        super.init(nibName: nil, bundle: nil)
        layoutAttribute = .bottom
        // The banner wraps; the standard titlebar height would clip it.
        automaticallyAdjustsSize = false
    }

    required init?(coder: NSCoder) { fatalError("IntegrityBannerAccessory is built in code") }

    override func loadView() {
        let container = ResizeReportingView(frame: NSRect(x: 0, y: 0, width: 800, height: 0))
        container.resized = { [weak self] in self?.measure() }
        addChild(banner)
        host.autoresizingMask = [.width, .height]
        host.frame = container.bounds
        container.addSubview(host)
        view = container
        observe()
        measure()
    }

    /// Follow what the banner shows; each change re-measures.
    private func observe() {
        withObservationTracking {
            _ = model.isClosed
            _ = model.health
            _ = model.healthError
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.measure()
                self?.observe()
            }
        }
    }

    /// Fit the accessory to the banner at the current width; hide it
    /// when the banner is empty.
    private func measure() {
        guard isViewLoaded else { return }
        let width = max(view.bounds.width, 1)
        let height = ceil(banner.sizeThatFits(in: NSSize(width: width, height: 10_000)).height)
        isHidden = height < 1
        guard abs(view.frame.height - height) >= 0.5 else { return }
        view.frame.size.height = height
        fullScreenMinHeight = height
    }
}

/// Reports its width changes, so the accessory can re-measure a
/// wrapping banner.
@MainActor
private final class ResizeReportingView: NSView {
    var resized: (() -> Void)?
    private var lastWidth: CGFloat = 0

    override func layout() {
        super.layout()
        guard abs(bounds.width - lastWidth) >= 0.5 else { return }
        lastWidth = bounds.width
        resized?()
    }
}
