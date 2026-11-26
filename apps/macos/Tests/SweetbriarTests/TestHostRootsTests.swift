#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
import AppKit
import SweetbriarBridge
import Testing

@Suite("Test host roots")
@MainActor
struct TestHostRootsTests {
    @Test func xcodesMarkersIdentifyATestHost() {
        #expect(TestHostRoots.isTestHost(["XCTestBundlePath": "/tmp/a.xctest"]))
        #expect(TestHostRoots.isTestHost(["XCTestSessionIdentifier": "a session"]))
        #expect(!TestHostRoots.isTestHost([:]))
        #expect(!TestHostRoots.isTestHost(["HOME": "/Users/example", "PATH": "/usr/bin"]))
    }

    @Test func privateRootsAreFreshSiblingsUnderTheirDirectory() throws {
        let directory = URL(fileURLWithPath: "/tmp/hosts", isDirectory: true)
        let first = TestHostRoots.make(in: directory)
        let second = TestHostRoots.make(in: directory)
        let parent = try #require(TestHostRoots.parent(of: first))
        #expect(first != second)
        #expect(parent != TestHostRoots.parent(of: second))
        #expect(parent.deletingLastPathComponent() == directory)
        #expect(URL(fileURLWithPath: first.dataRoot).deletingLastPathComponent() == parent)
        #expect(URL(fileURLWithPath: first.cacheRoot).deletingLastPathComponent() == parent)
        #expect(parent.lastPathComponent.hasPrefix(TestHostRoots.directoryPrefix))
        // Roots of any other layout name no directory to remove.
        #expect(TestHostRoots.parent(of: CoreConfig(dataRoot: "/tmp/elsewhere/data", cacheRoot: "/tmp/elsewhere/cache")) == nil)
    }

    /// This bundle runs inside the app (plan M0.10), so the running
    /// delegate is the one under test: it must have kept off the real
    /// archive.
    @Test func theRunningHostOpenedAPrivateArchive() throws {
        #expect(TestHostRoots.isTestHost())
        let delegate = try #require(NSApp.delegate as? AppDelegate)
        let roots = try #require(delegate.archiveRoots)
        let temporary = FileManager.default.temporaryDirectory
        let parent = try #require(TestHostRoots.parent(of: roots))
        #expect(parent.deletingLastPathComponent() == temporary)
        #expect(roots.dataRoot.hasPrefix(temporary.path))
        #expect(roots.cacheRoot.hasPrefix(temporary.path))
    }
}
