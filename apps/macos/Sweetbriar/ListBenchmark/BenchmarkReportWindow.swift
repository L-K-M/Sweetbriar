// The report window (spec/08 "Debug menu": "shows the numbers to
// copy"): the run's JSON in a selectable monospaced text view plus a
// Copy button. Shown after Debug → Run List Benchmark finishes; the
// launch-argument run writes the same report to its file instead.

import AppKit

@MainActor
final class BenchmarkReportWindowController: NSWindowController {
    /// The report's JSON, for the Copy button.
    private let reportText: String

    init(report: ListBenchmarkReport) {
        reportText = (try? report.encoded()).map {
            String(decoding: $0, as: UTF8.self)
        } ?? "{\n  \"ok\": false\n}"
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 720, height: 640),
            styleMask: [.titled, .closable, .miniaturizable, .resizable],
            backing: .buffered,
            defer: false
        )
        window.title = "List Benchmark"
        window.minSize = NSSize(width: 480, height: 320)
        let content = NSView(
            frame: NSRect(x: 0, y: 0, width: 720, height: 640)
        )
        window.contentView = content

        // AppKit's preconfigured pair: the text view tracks the
        // scroll view's width and grows with its text.
        let scrollView = NSTextView.scrollableTextView()
        scrollView.translatesAutoresizingMaskIntoConstraints = false
        if let textView = scrollView.documentView as? NSTextView {
            textView.isEditable = false
            textView.isSelectable = true
            textView.isRichText = false
            textView.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
            textView.string = reportText
        }
        content.addSubview(scrollView)

        let copy = NSButton(
            title: "Copy",
            target: nil,
            action: #selector(copyReport(_:))
        )
        copy.bezelStyle = .rounded
        copy.translatesAutoresizingMaskIntoConstraints = false
        copy.keyEquivalent = "\r"
        content.addSubview(copy)

        NSLayoutConstraint.activate([
            scrollView.topAnchor.constraint(equalTo: content.topAnchor),
            scrollView.leadingAnchor.constraint(
                equalTo: content.leadingAnchor
            ),
            scrollView.trailingAnchor.constraint(
                equalTo: content.trailingAnchor
            ),
            scrollView.bottomAnchor.constraint(
                equalTo: copy.topAnchor, constant: -8
            ),
            copy.bottomAnchor.constraint(
                equalTo: content.bottomAnchor, constant: -8
            ),
            copy.trailingAnchor.constraint(
                equalTo: content.trailingAnchor, constant: -12
            ),
        ])
        super.init(window: window)
        copy.target = self
        window.center()
    }

    required init?(coder: NSCoder) {
        fatalError("BenchmarkReportWindowController is built in code")
    }

    @objc
    private func copyReport(_ sender: NSButton) {
        NSPasteboard.general.clearContents()
        NSPasteboard.general.setString(reportText, forType: .string)
    }
}
