import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@MainActor
private func imageEventually(_ predicate: @MainActor () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !predicate() {
        if ContinuousClock.now >= deadline || Task.isCancelled { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

/// A 2×2 image as `type` data.
@MainActor
private func imageData(_ type: NSBitmapImageRep.FileType) throws -> Data {
    let rep = try #require(
        NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: 2, pixelsHigh: 2, bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
            bytesPerRow: 0, bitsPerPixel: 0))
    return try #require(rep.representation(using: type, properties: [:]))
}

@MainActor
private func imageFile(_ name: String) throws -> URL {
    let directory = FileManager.default.temporaryDirectory.appendingPathComponent(
        "sb-image-\(UUID().uuidString)", isDirectory: true)
    try FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
    let file = directory.appendingPathComponent(name)
    try imageData(.png).write(to: file)
    return file
}

/// A draft in `format` holding `body`, open in a window whose editor has
/// focus and its caret at the end, over an empty private pasteboard.
@MainActor
private func withImageWindow(
    format: ComposeFormat = .markdown, body: String = "Hello ",
    test: @MainActor (ComposeWindowController, CoreBridge, NSPasteboard) async throws -> Void
) async throws {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("sb-inline-\(UUID().uuidString)")
    let bridge = try await CoreBridge.open(
        config: CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path),
        snapshots: FrameBatcher<[Setting]>(clock: ImageClock()) { _ in })
    let pasteboard = NSPasteboard(name: NSPasteboard.Name("sb-inline-\(UUID().uuidString)"))
    pasteboard.clearContents()
    var controller: ComposeWindowController?
    let result: Result<Void, any Error>
    do {
        let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: created.draftId, baseRevisionId: nil,
            content: DraftContent(
                identityId: nil, format: format, plainFlowed: true, subject: "Images",
                to: [], cc: [], bcc: [], bodySource: body, attachments: []))
        let window = ComposeWindowController(source: bridge, draftId: created.draftId, pasteboard: pasteboard)
        controller = window
        window.showWindow(nil)
        let mode: ComposerMode = format == .plain ? .plain : .markdown
        try #require(
            await imageEventually {
                window.textView.isEditable && window.textView.string == body && window.textView.mode == mode
            })
        window.window?.makeFirstResponder(window.textView)
        window.textView.setSelectedRange(NSRange(location: (body as NSString).length, length: 0))
        try await test(window, bridge, pasteboard)
        result = .success(())
    } catch { result = .failure(error) }
    if let controller, !(await controller.closeAndWait()) {
        _ = await controller.closeWithoutSavingAndWait()
    }
    pasteboard.releaseGlobally()
    await bridge.shutdown()
    try? FileManager.default.removeItem(at: root)
    try result.get()
}

private struct ImageClock: FrameClock { func nextFrame() async { await Task.yield() } }

@MainActor
private func rowLabels(_ controller: ComposeWindowController) -> [String] {
    controller.attachmentControls.list.views.compactMap {
        (($0 as? NSStackView)?.views.first as? NSTextField)?.stringValue
    }
}

/// Inline images in the composer (spec/08 "Inline images", M4.4e3).
@Suite(.serialized, .timeLimit(.minutes(1))) @MainActor
struct InlineImageComposeTests {
    @Test func theEditMenuOffersInsertImage() throws {
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {})
        let edit = try #require(builder.makeMainMenu().items.compactMap(\.submenu).first { $0.title == "Edit" })
        let item = try #require(edit.items.first { $0.action == #selector(ComposeWindowController.insertImage(_:)) })
        #expect(item.title == "Insert Image…")
        #expect(item.keyEquivalent.isEmpty)
    }

    /// The image joins with a fresh Content-ID, its reference goes in at
    /// the caret, its row says Inline, and the two undo and redo as one
    /// edit. The save stores the Content-ID.
    @Test func anInsertedImageAndItsReferenceAreOneUndoableEdit() async throws {
        try await withImageWindow { controller, bridge, _ in
            let undo = try #require(controller.window?.undoManager)
            let menuItem = NSMenuItem(
                title: "Insert Image…", action: #selector(ComposeWindowController.insertImage(_:)),
                keyEquivalent: "")
            #expect(controller.validateMenuItem(menuItem))
            #expect(controller.attachmentControls.insertImages([try imageFile("Chart [Q3].png")]))
            try #require(await imageEventually { controller.model.content?.attachments.count == 1 })

            let attachment = try #require(controller.model.content?.attachments.first)
            let contentId = try #require(attachment.contentId)
            let reference = #"![Chart \[Q3\]](cid:\#(contentId))"#
            #expect(controller.textView.string == "Hello " + reference)
            #expect(rowLabels(controller) == ["Chart [Q3].png (Inline)"])
            #expect(undo.undoActionName == "Insert Image")

            undo.undo()
            #expect(controller.textView.string == "Hello ")
            #expect(controller.model.content?.attachments.isEmpty == true)
            undo.redo()
            #expect(controller.textView.string == "Hello " + reference)
            #expect(controller.model.content?.attachments.first?.contentId == contentId)

            #expect(await controller.saveAndWait())
            let stored = try #require(try await bridge.draft(id: controller.model.draftId).revision).attachments
            #expect(stored.map(\.contentId) == [contentId])
            #expect(stored.map(\.mediaType) == ["image/png"])
            #expect(try await bridge.draft(id: controller.model.draftId).revision?.bodySource
                == "Hello " + reference)
        }
    }

    /// Pasted TIFF data is converted to PNG and named Image; ⌘V is
    /// enabled for it.
    @Test func pastedTiffDataInsertsAPNGNamedImage() async throws {
        try await withImageWindow { controller, bridge, pasteboard in
            pasteboard.clearContents()
            pasteboard.setData(try imageData(.tiff), forType: .tiff)
            #expect(controller.canPasteImage())
            let paste = NSMenuItem(title: "Paste", action: #selector(NSText.paste(_:)), keyEquivalent: "v")
            #expect(controller.textView.validateMenuItem(paste))

            controller.textView.paste(nil)
            try #require(await imageEventually { controller.model.content?.attachments.count == 1 })
            let contentId = try #require(controller.model.content?.attachments.first?.contentId)
            #expect(controller.textView.string == "Hello ![Image](cid:\(contentId))")
            #expect(rowLabels(controller) == ["Image.png (Inline)"])

            #expect(await controller.saveAndWait())
            let stored = try #require(
                try await bridge.draft(id: controller.model.draftId).revision?.attachments.first)
            #expect(stored.filename == "Image.png" && stored.mediaType == "image/png")
        }
    }

    /// Text on the pasteboard always pastes as text, so image data next
    /// to it isn't taken; PNG and JPEG data alone are.
    @Test func onlyImageDataWithoutTextPastesAsAnImage() throws {
        let pasteboard = NSPasteboard(name: NSPasteboard.Name("sb-inline-\(UUID().uuidString)"))
        defer { pasteboard.releaseGlobally() }
        let png = try imageData(.png)
        pasteboard.clearContents()
        pasteboard.setData(png, forType: .png)
        pasteboard.setString("caption", forType: .string)
        #expect(!ComposeAttachmentControls.holdsPastedImage(pasteboard))
        #expect(ComposeAttachmentControls.pastedImage(on: pasteboard) == nil)

        pasteboard.clearContents()
        pasteboard.setData(png, forType: .png)
        #expect(ComposeAttachmentControls.pastedImage(on: pasteboard)?.mediaType == "image/png")
        #expect(ComposeAttachmentControls.pastedImage(on: pasteboard)?.data == png)

        let jpeg = try imageData(.jpeg)
        pasteboard.clearContents()
        pasteboard.setData(jpeg, forType: NSPasteboard.PasteboardType("public.jpeg"))
        #expect(ComposeAttachmentControls.pastedImage(on: pasteboard)?.mediaType == "image/jpeg")
    }

    /// Removing the Inline row keeps the reference text.
    @Test func removingTheInlineRowKeepsTheReference() async throws {
        try await withImageWindow { controller, _, _ in
            #expect(controller.attachmentControls.insertImages([try imageFile("Chart.png")]))
            try #require(await imageEventually { controller.model.content?.attachments.count == 1 })
            let text = controller.textView.string
            controller.attachmentControls.remove(at: 0)
            #expect(controller.model.content?.attachments.isEmpty == true)
            #expect(controller.textView.string == text)
            #expect(text.hasPrefix("Hello ![Chart](cid:"))
        }
    }

    /// In a plain draft Insert Image… is disabled and image data isn't
    /// taken.
    @Test func aPlainDraftTakesNoInlineImages() async throws {
        try await withImageWindow(format: .plain) { controller, _, pasteboard in
            let menuItem = NSMenuItem(
                title: "Insert Image…", action: #selector(ComposeWindowController.insertImage(_:)),
                keyEquivalent: "")
            #expect(!controller.validateMenuItem(menuItem))
            #expect(!controller.attachmentControls.insertImages([try imageFile("Chart.png")]))

            pasteboard.clearContents()
            pasteboard.setData(try imageData(.png), forType: .png)
            #expect(!controller.attachmentControls.pasteImage(from: pasteboard))
            #expect(controller.attachmentControls.publications.publications.isEmpty)
            #expect(controller.model.content?.attachments.isEmpty == true)
            #expect(controller.textView.string == "Hello ")
        }
    }
}
