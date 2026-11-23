// A message in its own window (spec/08 "Message window"): Return and
// Space in the list, one window per occurrence, its title, paging the
// reader, closing them all, and the Message menu's Open in New Window.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import Carbon.HIToolbox
import Foundation
import SweetbriarBridge
import Testing

/// One plain-text message, long enough to page.
private struct LongMessageSource: ReaderSource {
    let directory: URL
    let subject: String?

    init(subject: String?) throws {
        self.subject = subject
        directory = FileManager.default.temporaryDirectory
            .appendingPathComponent(UUID().uuidString)
        try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        let lines = (1...400).map { "Line \($0) of a long synthetic message." }
        try Data(lines.joined(separator: "\n").utf8)
            .write(to: directory.appendingPathComponent("body.txt"))
    }

    func messageForOccurrence(occurrenceId: Int64) async throws -> Int64 { 7 }
    func getMessage(messageId: Int64) async throws -> ReaderMessage {
        ReaderMessage(
            messageId: 7,
            headers: .init(
                from: [.init(name: "Sender", address: "sender@example.test")],
                to: [.init(name: nil, address: "to@example.test")], cc: [], date: 1_792_238_400_000,
                subject: subject, messageId: nil),
            accountId: "local", accountDisplayName: "Local Archive", occurrences: [],
            threadId: nil, tags: [], note: nil, bodyAvailability: .acquired)
    }
    func openBody(messageID: Int64, preferred: BodyKind) async throws -> BodyLease {
        BodyLease(
            leaseId: "long", kind: .plainText,
            filePath: directory.appendingPathComponent("body.txt").path, byteLength: 0,
            remoteContentBlocked: 0, warnings: [], manifestPath: nil)
    }
    func openAttachment(messageID: Int64, partID: String) async throws -> AttachmentLease {
        throw CoreFailure.notFound(what: "attachment")
    }
    func releaseLease(_ leaseID: String) {}
}

@MainActor
private func reader(subject: String? = "Synthetic subject") throws -> ReaderViewController {
    ReaderViewController(
        model: ReaderModel(source: try LongMessageSource(subject: subject), preferred: .plainText))
}

private func key(_ code: Int, _ characters: String, _ flags: NSEvent.ModifierFlags = []) -> NSEvent {
    NSEvent.keyEvent(
        with: .keyDown, location: .zero, modifierFlags: flags, timestamp: 0, windowNumber: 0,
        context: nil, characters: characters, charactersIgnoringModifiers: characters,
        isARepeat: false, keyCode: UInt16(code))!
}

@MainActor
private func waitUntil(_ condition: () -> Bool) async throws {
    for _ in 0..<500 where !condition() {
        try await Task.sleep(for: .milliseconds(10))
    }
}

@Suite("message window", .serialized)
@MainActor
struct MessageWindowTests {
    /// Return and keypad Enter open; Space pages down and ⇧Space up;
    /// with ⌘, ⌥ or ⌃ held, or another key, the table keeps the key.
    @Test func returnOpensAndSpacePagesTheReader() {
        #expect(MessageTableView.command(key(kVK_Return, "\r")) == .open)
        #expect(MessageTableView.command(key(kVK_ANSI_KeypadEnter, "\u{3}")) == .open)
        #expect(MessageTableView.command(key(kVK_Space, " ")) == .page(.down))
        #expect(MessageTableView.command(key(kVK_Space, " ", .shift)) == .page(.up))
        #expect(MessageTableView.command(key(kVK_Return, "\r", .shift)) == nil)
        #expect(MessageTableView.command(key(kVK_Space, " ", .command)) == nil)
        #expect(MessageTableView.command(key(kVK_Return, "\r", .option)) == nil)
        #expect(MessageTableView.command(key(kVK_ANSI_A, "a")) == nil)
    }

    /// A second open of the same occurrence brings its window forward
    /// instead of making another; closing one forgets it, and closing
    /// all, as a restore does, closes every window.
    @Test(.timeLimit(.minutes(1))) func oneWindowPerMessageUntilTheContentRetires() async throws {
        let windows = MessageWindows()
        var made = 0
        let make = { (id: Int64) -> MessageWindowController in
            made += 1
            return MessageWindowController(
                occurrenceId: id, reader: try! reader(), markRead: nil, target: nil)
        }
        let first = windows.open(11) { make(11) }
        let again = windows.open(11) { make(11) }
        #expect(first === again)
        #expect(made == 1)
        let other = windows.open(12) { make(12) }
        #expect(windows.controllers.count == 2)
        #expect(windows.occurrence(of: other.window) == 12)
        // The second window cascades rather than covering the first.
        #expect(other.window?.frame.origin != first.window?.frame.origin)

        other.close()
        #expect(windows.controllers.keys.sorted() == [11])

        try await waitUntil { first.window?.title == "Synthetic subject" }
        #expect(first.window?.title == "Synthetic subject")
        #expect(first.window?.isRestorable == false)

        windows.closeAll()
        #expect(windows.controllers.isEmpty)
        #expect(first.window?.isVisible == false)
    }

    /// A window without a subject, or with a blank one, says so.
    @Test func aMessageWithoutASubjectIsTitledNoSubject() {
        #expect(MessageWindowController.title(nil) == "No Subject")
        #expect(MessageWindowController.title("  \n") == "No Subject")
        #expect(MessageWindowController.title("Hello") == "Hello")
    }

    /// Space pages the shown plain-text body down, ⇧Space back up.
    @Test(.timeLimit(.minutes(1))) func spacePagesTheShownBody() async throws {
        let controller = try reader()
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 480, height: 360),
            styleMask: [.titled, .resizable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentViewController = controller
        // The controller's view starts at zero size, which the window
        // takes on; give it the room a reader has.
        window.setContentSize(NSSize(width: 480, height: 360))
        defer {
            controller.close()
            window.close()
        }
        await controller.show(selection: [11])
        window.layoutIfNeeded()
        let text = controller.plainText
        if let layout = text.textLayoutManager {
            layout.ensureLayout(for: layout.documentRange)
        }
        text.sizeToFit()
        window.layoutIfNeeded()
        let clip = controller.textScroll.contentView
        try #require(!controller.textScroll.isHidden)
        try #require(clip.bounds.height > 0, "the body has room on screen")
        try #require(
            text.frame.height > clip.bounds.height * 2,
            "the body is taller than two pages: \(text.frame.height) in \(clip.bounds.height)")
        let top = clip.bounds.origin.y

        controller.page(.down)
        let paged = clip.bounds.origin.y
        #expect(paged > top, "a page down moves the body")

        controller.page(.up)
        #expect(clip.bounds.origin.y < paged, "a page up moves it back")
    }

    /// Message → Open in New Window is ⌘O, nil-target, with its command
    /// identifier, so it reaches the main window down the responder chain.
    @Test func openInNewWindowIsANilTargetCommand() throws {
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {}
        )
        let menu = builder.makeMainMenu()
        let message = try #require(menu.items.compactMap(\.submenu).first { $0.title == "Message" })
        let open = try #require(message.items.first { $0.title == "Open in New Window" })
        #expect(open.identifier?.rawValue == "ch.lkmc.Sweetbriar.cmd.openMessage")
        #expect(open.action.map(NSStringFromSelector) == "openMessage:")
        #expect(open.target == nil)
        #expect(open.keyEquivalent == "o" && open.keyEquivalentModifierMask == .command)
    }
}
#endif
