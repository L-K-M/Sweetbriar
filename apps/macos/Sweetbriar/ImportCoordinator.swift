// The app's import flow (spec/09 "In the app"): File → Import… picks
// files, packages and folders, starts the job and shows the progress
// sheet on the main window; the Import report window shows a written
// report. The active job's id is kept in user defaults, so a restarted
// app reattaches; it is dropped once the user is done with a finished
// job.

import AppKit
import SweetbriarBridge
import UniformTypeIdentifiers

@MainActor
final class ImportCoordinator {
    /// Where the active job's id is kept across launches.
    private static let activeJobKey = "import.activeJobId"

    private let bridge: CoreBridge
    private weak var window: NSWindow?
    private var session: ImportSession?
    private var sheet: NSWindow?
    private var reportWindows: [ImportReportWindowController] = []

    /// The job changed what the archive holds: the sidebar re-reads.
    var onArchiveChanged: (@MainActor () -> Void)?

    init(bridge: CoreBridge, window: NSWindow) {
        self.bridge = bridge
        self.window = window
    }

    /// File → Import…: choose what to import, then start the job. With
    /// a job already under way, show its sheet again instead.
    func chooseAndImport() async {
        guard let window else { return }
        if let session, let jobId = session.jobId {
            guard sheet == nil else { return }
            session.attach(jobId: jobId)
            showSheet()
            return
        }
        let panel = NSOpenPanel()
        panel.canChooseFiles = true
        panel.canChooseDirectories = true
        panel.allowsMultipleSelection = true
        panel.treatsFilePackagesAsDirectories = false
        panel.prompt = "Import"
        panel.message = "Choose mailboxes (.mbox), messages (.eml) or folders holding them."
        panel.allowedContentTypes = [UTType(filenameExtension: "mbox"), UTType(filenameExtension: "eml"), .folder]
            .compactMap { $0 }
        guard await panel.beginSheetModal(for: window) == .OK else { return }
        await start(paths: panel.urls.map { $0.path(percentEncoded: false) })
    }

    /// Start a job over `paths` and show its sheet.
    func start(paths: [String]) async {
        let session = makeSession()
        do {
            try await session.start(paths: paths)
        } catch {
            self.session = nil
            presentFailure("The import couldn't start", error)
            return
        }
        UserDefaults.standard.set(session.jobId, forKey: Self.activeJobKey)
        showSheet()
    }

    /// Reattach to the job a previous launch left, if any (spec/09).
    func reattach() {
        guard session == nil,
              let id = UserDefaults.standard.string(forKey: Self.activeJobKey)
        else {
            return
        }
        let session = makeSession()
        session.attach(jobId: id)
        showSheet()
    }

    private func makeSession() -> ImportSession {
        let session = ImportSession(source: bridge)
        session.onJobChanged = { [weak self] _ in
            self?.onArchiveChanged?()
        }
        self.session = session
        return session
    }

    private func showSheet() {
        guard let window, let session else { return }
        let controller = ImportSheetController(session: session)
        controller.onClose = { [weak self] in
            self?.closeSheet()
        }
        controller.onShowReport = { [weak self] report in
            self?.showReport(report)
        }
        let sheet = NSWindow(contentViewController: controller)
        self.sheet = sheet
        window.beginSheet(sheet)
    }

    /// Close the sheet. An unfinished job runs on and stays kept for a
    /// reattach; a finished one is let go.
    private func closeSheet() {
        if let sheet {
            window?.endSheet(sheet)
        }
        sheet = nil
        guard let session else { return }
        session.detach()
        if session.job?.state.isFinished ?? false {
            UserDefaults.standard.removeObject(forKey: Self.activeJobKey)
            self.session = nil
        }
        onArchiveChanged?()
    }

    private func showReport(_ report: ImportReport) {
        let controller = ImportReportWindowController(report: report)
        controller.onClose = { [weak self, weak controller] in
            self?.reportWindows.removeAll { $0 === controller }
        }
        reportWindows.append(controller)
        controller.showWindow(nil)
    }

    private func presentFailure(_ title: String, _ error: any Error) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = error.localizedDescription
        alert.alertStyle = .warning
        if let window {
            alert.beginSheetModal(for: window, completionHandler: nil)
        } else {
            _ = alert.runModal()
        }
    }
}

/// The Import report window (spec/08 "Windows", spec/09 "Report"): the
/// report's path, a button to show it in Finder, and its JSON.
@MainActor
final class ImportReportWindowController: NSWindowController, NSWindowDelegate {
    /// The window's accessibility identifier, for UI tests.
    static let accessibilityID = "importReport"
    private static let contentSize = NSSize(width: 640, height: 480)

    private let report: ImportReport
    var onClose: (@MainActor () -> Void)?

    init(report: ImportReport) {
        self.report = report
        let window = NSWindow(
            contentRect: NSRect(origin: .zero, size: Self.contentSize),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "Import Report"
        window.isReleasedWhenClosed = false
        window.setAccessibilityIdentifier(Self.accessibilityID)
        super.init(window: window)
        window.delegate = self
        window.contentView = makeContent()
        window.center()
    }

    required init?(coder: NSCoder) {
        fatalError("ImportReportWindowController is built in code")
    }

    func windowWillClose(_ notification: Notification) {
        onClose?()
    }

    private func makeContent() -> NSView {
        let path = NSTextField(labelWithString: report.path)
        path.lineBreakMode = .byTruncatingMiddle
        path.isSelectable = true
        let reveal = NSButton(title: "Show in Finder", target: self, action: #selector(reveal(_:)))

        let text = NSTextView()
        text.isEditable = false
        text.font = .monospacedSystemFont(ofSize: 12, weight: .regular)
        text.string = Self.pretty(report.json)
        text.isVerticallyResizable = true
        text.autoresizingMask = [.width]
        let scroll = NSScrollView()
        scroll.documentView = text
        scroll.hasVerticalScroller = true
        scroll.borderType = .bezelBorder

        let header = NSStackView(views: [path, reveal])
        header.orientation = .horizontal
        let stack = NSStackView(views: [header, scroll])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.edgeInsets = NSEdgeInsets(top: 16, left: 16, bottom: 16, right: 16)
        stack.translatesAutoresizingMaskIntoConstraints = false

        let content = NSView()
        content.addSubview(stack)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            scroll.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
            header.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -32),
        ])
        return content
    }

    /// The JSON re-indented for reading; as written when it doesn't
    /// parse.
    private static func pretty(_ json: String) -> String {
        guard let object = try? JSONSerialization.jsonObject(with: Data(json.utf8)),
              let data = try? JSONSerialization.data(
                  withJSONObject: object,
                  options: [.prettyPrinted, .sortedKeys]
              ),
              let text = String(data: data, encoding: .utf8)
        else {
            return json
        }
        return text
    }

    @objc
    private func reveal(_ sender: Any?) {
        NSWorkspace.shared.activateFileViewerSelecting([URL(fileURLWithPath: report.path)])
    }
}
