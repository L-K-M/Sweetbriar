// The import progress sheet (spec/09 "In the app"): the inventory and
// its problems before confirmation, a variant choice per ambiguous
// source, then progress, the outcome and the report. It renders an
// `ImportSession` and re-renders whenever the session changes.
// Closing it leaves the job running; Cancel waits for the receipt.

import AppKit
import Observation
import SweetbriarBridge

@MainActor
final class ImportSheetController: NSViewController {
    /// Accessibility identifiers, for UI tests.
    enum ID {
        static let sheet = "importSheet"
        static let confirm = "importConfirm"
        static let cancel = "importCancel"
        static let close = "importClose"
        static let showReport = "importShowReport"
        static let retryReport = "importRetryReport"
    }

    private static let width: CGFloat = 520
    /// Problems listed in the sheet; the report has them all.
    private static let listedProblems = 5

    private let session: ImportSession
    private let headline = NSTextField(labelWithString: "")
    private let detail = NSTextField(wrappingLabelWithString: "")
    private let progress = NSProgressIndicator()
    private let choices = NSStackView()
    private let problems = NSTextField(wrappingLabelWithString: "")
    private let reportStatus = NSTextField(labelWithString: "")
    private let confirmButton = NSButton(title: "Import", target: nil, action: nil)
    private let cancelButton = NSButton(title: "Cancel Import", target: nil, action: nil)
    private let closeButton = NSButton(title: "Close", target: nil, action: nil)
    private let showReportButton = NSButton(title: "Show Report", target: nil, action: nil)
    private let retryButton = NSButton(title: "Retry", target: nil, action: nil)

    /// The ambiguous sources the choice rows were built for.
    private var choiceOrdinals: [UInt64] = []

    /// Close the sheet; the job runs on.
    var onClose: (@MainActor () -> Void)?
    /// Show the written report.
    var onShowReport: (@MainActor (ImportReport) -> Void)?

    init(session: ImportSession) {
        self.session = session
        super.init(nibName: nil, bundle: nil)
    }

    required init?(coder: NSCoder) {
        fatalError("ImportSheetController is built in code")
    }

    override func loadView() {
        headline.font = .boldSystemFont(ofSize: 15)
        detail.textColor = .secondaryLabelColor
        problems.textColor = .secondaryLabelColor
        reportStatus.textColor = .secondaryLabelColor
        progress.isIndeterminate = false
        progress.minValue = 0
        progress.style = .bar
        choices.orientation = .vertical
        choices.alignment = .leading
        choices.spacing = 6

        for (button, action, id) in [
            (confirmButton, #selector(confirm(_:)), ID.confirm),
            (cancelButton, #selector(cancelImport(_:)), ID.cancel),
            (closeButton, #selector(close(_:)), ID.close),
            (showReportButton, #selector(showReport(_:)), ID.showReport),
            (retryButton, #selector(retryReport(_:)), ID.retryReport),
        ] {
            button.target = self
            button.action = action
            button.setAccessibilityIdentifier(id)
        }
        confirmButton.keyEquivalent = "\r"

        let buttons = NSStackView(views: [cancelButton, closeButton, showReportButton, confirmButton])
        buttons.orientation = .horizontal
        buttons.spacing = 8
        let report = NSStackView(views: [reportStatus, retryButton])
        report.orientation = .horizontal

        let stack = NSStackView(views: [headline, detail, progress, choices, problems, report, buttons])
        stack.orientation = .vertical
        stack.alignment = .leading
        stack.spacing = 12
        stack.edgeInsets = NSEdgeInsets(top: 20, left: 20, bottom: 20, right: 20)
        stack.translatesAutoresizingMaskIntoConstraints = false
        buttons.setHuggingPriority(.defaultLow, for: .horizontal)

        let content = NSView()
        content.addSubview(stack)
        content.setAccessibilityIdentifier(ID.sheet)
        NSLayoutConstraint.activate([
            stack.leadingAnchor.constraint(equalTo: content.leadingAnchor),
            stack.trailingAnchor.constraint(equalTo: content.trailingAnchor),
            stack.topAnchor.constraint(equalTo: content.topAnchor),
            stack.bottomAnchor.constraint(equalTo: content.bottomAnchor),
            content.widthAnchor.constraint(equalToConstant: Self.width),
            progress.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -40),
            detail.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -40),
            problems.widthAnchor.constraint(equalTo: stack.widthAnchor, constant: -40),
        ])
        view = content
        observe()
    }

    /// Render, and render again on the next change to anything the
    /// render read.
    private func observe() {
        withObservationTracking {
            render()
        } onChange: { [weak self] in
            Task { @MainActor [weak self] in
                self?.observe()
            }
        }
    }

    private func render() {
        let job = session.job
        let state = job?.state
        headline.stringValue = Self.headline(state)
        detail.stringValue = job.map(Self.detail) ?? "Looking at what you chose…"

        let messages = job?.sources.reduce(0) { $0 + $1.messages } ?? 0
        let done = job?.sources.reduce(0) { $0 + $1.messagesDone } ?? 0
        progress.isHidden = state != .running
        progress.maxValue = Double(max(messages, 1))
        progress.doubleValue = Double(done)

        renderChoices(state == .awaitingConfirmation ? session.ambiguousSources : [])
        renderProblems(job)

        let finished = state?.isFinished ?? false
        switch session.report {
        case .notFinished:
            reportStatus.stringValue = ""
            retryButton.isHidden = true
            showReportButton.isHidden = true
        case .pending:
            reportStatus.stringValue = "The report isn't written yet."
            retryButton.isHidden = false
            showReportButton.isHidden = true
        case .written:
            reportStatus.stringValue = ""
            retryButton.isHidden = true
            showReportButton.isHidden = false
        }
        if let error = session.lastError {
            reportStatus.stringValue = "Couldn't reach the import: \(error.localizedDescription)"
        }

        confirmButton.isHidden = state != .awaitingConfirmation
        confirmButton.isEnabled = session.canConfirm
        cancelButton.isHidden = finished || job == nil
        cancelButton.isEnabled = !session.cancelling
        closeButton.title = finished ? "Done" : "Close"
    }

    private static func headline(_ state: ImportJobState?) -> String {
        switch state {
        case nil, .inventory:
            return "Taking inventory…"
        case .awaitingConfirmation:
            return "Ready to import"
        case .running:
            return "Importing…"
        case .completed:
            return "Import finished"
        case .cancelled:
            return "Import cancelled"
        case .failed:
            return "Import failed"
        }
    }

    /// The counts that matter in each state; a failed or cancelled job
    /// says its committed messages stay (spec/09).
    private static func detail(_ job: ImportJob) -> String {
        let sources = job.sources.count
        let messages = job.sources.reduce(0) { $0 + $1.messages }
        let totals = job.totals
        switch job.state {
        case .inventory:
            return "Looking at what you chose…"
        case .awaitingConfirmation:
            return "\(sources) \(sources == 1 ? "source" : "sources"), \(messages) messages."
        case .running:
            return "\(totals.processed) of \(messages) messages."
        case .completed:
            return "\(totals.imported) imported, \(totals.duplicates) already in the archive, "
                + "\(totals.rejected) unreadable."
        case .cancelled:
            return "\(totals.imported) messages imported before the cancel stay in the archive."
        case .failed:
            let reason = job.failure ?? "unknown"
            return "\(reason). \(totals.imported) messages imported before it stay in the archive."
        }
    }

    /// One row per ambiguous source: its mailbox and a variant popup,
    /// rebuilt only when the sources change (spec/09: an ambiguous
    /// source needs its own choice).
    private func renderChoices(_ sources: [ImportSource]) {
        let ordinals = sources.map(\.ordinal)
        guard ordinals != choiceOrdinals else { return }
        choiceOrdinals = ordinals
        for view in choices.arrangedSubviews {
            choices.removeArrangedSubview(view)
            view.removeFromSuperview()
        }
        guard !sources.isEmpty else {
            choices.isHidden = true
            return
        }
        choices.isHidden = false
        let note = NSTextField(wrappingLabelWithString:
            "These sources have lines starting “>From”, which mbox variants read differently. Choose how to read each."
        )
        note.textColor = .secondaryLabelColor
        choices.addArrangedSubview(note)
        for source in sources {
            let label = NSTextField(labelWithString: source.mailbox)
            label.toolTip = source.path
            let popup = NSPopUpButton(frame: .zero, pullsDown: false)
            popup.addItems(withTitles: [
                "Choose…",
                "Unquote “>From” lines (mboxrd)",
                "Keep “>From” lines (mboxo)",
            ])
            popup.tag = Int(source.ordinal)
            popup.target = self
            popup.action = #selector(variantChosen(_:))
            popup.setAccessibilityIdentifier("importVariant-\(source.ordinal)")
            let row = NSStackView(views: [label, popup])
            row.orientation = .horizontal
            choices.addArrangedSubview(row)
        }
    }

    private func renderProblems(_ job: ImportJob?) {
        guard let job, job.problemCount > 0 else {
            problems.isHidden = true
            return
        }
        problems.isHidden = false
        let listed = job.problems.prefix(Self.listedProblems).map { problem in
            "\(problem.path): \(problem.detail)"
        }
        let more = Int(job.problemCount) - listed.count
        var lines = ["\(job.problemCount) \(job.problemCount == 1 ? "item" : "items") won't be imported:"]
        lines += listed
        if more > 0 {
            lines.append("and \(more) more, listed in the report")
        }
        problems.stringValue = lines.joined(separator: "\n")
    }

    @objc
    private func variantChosen(_ sender: NSPopUpButton) {
        let ordinal = UInt64(sender.tag)
        switch sender.indexOfSelectedItem {
        case 1:
            session.choose(.mboxrd, forSource: ordinal)
        case 2:
            session.choose(.mboxo, forSource: ordinal)
        default:
            break
        }
    }

    @objc
    private func confirm(_ sender: Any?) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                try await session.confirm()
            } catch {
                presentFailure("The import couldn't start", error)
            }
        }
    }

    @objc
    private func cancelImport(_ sender: Any?) {
        Task { @MainActor [weak self] in
            guard let self else { return }
            do {
                try await session.cancel()
            } catch {
                presentFailure("The import couldn't be cancelled", error)
            }
        }
    }

    @objc
    private func close(_ sender: Any?) {
        onClose?()
    }

    @objc
    private func showReport(_ sender: Any?) {
        guard case .written(let report) = session.report else { return }
        onShowReport?(report)
    }

    @objc
    private func retryReport(_ sender: Any?) {
        session.retryReport()
    }

    private func presentFailure(_ title: String, _ error: any Error) {
        let alert = NSAlert()
        alert.messageText = title
        alert.informativeText = error.localizedDescription
        alert.alertStyle = .warning
        if let window = view.window {
            alert.beginSheetModal(for: window, completionHandler: nil)
        } else {
            _ = alert.runModal()
        }
    }
}
