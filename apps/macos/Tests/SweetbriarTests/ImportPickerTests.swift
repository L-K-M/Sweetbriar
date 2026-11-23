import AppKit
import Testing

@testable import Sweetbriar

/// spec/09 "In the app": File → Import… picks files, packages and
/// folders, several at once, and filters nothing by extension, since a
/// chosen file without `.eml` is an mbox candidate.
@MainActor
@Suite
struct ImportPickerTests {
    @Test
    func thePickerTakesAnyFilePackageOrFolder() {
        let panel = NSOpenPanel()
        ImportCoordinator.configure(panel)
        #expect(panel.allowedContentTypes.isEmpty)
        #expect(panel.canChooseFiles)
        #expect(panel.canChooseDirectories)
        #expect(panel.allowsMultipleSelection)
        #expect(!panel.treatsFilePackagesAsDirectories)
        #expect(panel.prompt == "Import")
    }
}
