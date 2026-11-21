import Foundation
import Testing

@testable import SweetbriarBridge

@Suite
struct PasteConversionBridgeTests {
    @Test(.timeLimit(.minutes(1))) @MainActor
    func theRealCoreConvertsNamesLossesAndRefusesWhole() async throws {
        let (root, config) = tempConfig()
        defer { try? FileManager.default.removeItem(at: root) }
        let core = try await CoreBridge.open(
            config: config, snapshots: sinkBatcher(clock: ManualFrameClock()))
        do {
            let source: any PasteConversionSource = core
            #expect(
                try await source.markdownFromHtml(html: "<h2>Plan</h2><ul><li><em>one</em></li></ul>")
                    == MarkdownPaste(markdown: "## Plan\n\n- *one*", losses: []))
            #expect(
                try await source.markdownFromHtml(
                    html: "<table><tr><td colspan=2>a</td></tr></table><font color=red>b</font>"
                ).losses == [.styling, .tables])
            do {
                _ = try await core.markdownFromHtml(html: String(repeating: "a", count: 2 * 1024 * 1024 + 1))
                Issue.record("too large HTML must refuse, not truncate")
            } catch let failure as CoreFailure { guard case .invalidInput = failure else { throw failure } }
            let cancelled = Task {
                withUnsafeCurrentTask { $0?.cancel() }
                return try await core.markdownFromHtml(html: "<p>a</p>")
            }
            do {
                _ = try await cancelled.value
                Issue.record("cancel must throw")
            } catch is CancellationError {}
            await core.shutdown()
            do {
                _ = try await core.markdownFromHtml(html: "<p>a</p>")
                Issue.record("closed must refuse")
            } catch let failure as CoreFailure { guard case .unavailable = failure else { throw failure } }
        } catch {
            await core.shutdown()
            throw error
        }
    }
}
