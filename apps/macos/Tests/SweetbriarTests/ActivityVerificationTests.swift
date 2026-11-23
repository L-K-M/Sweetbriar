// Verify… and Try Again in the Activity window (spec/08 "Verifying
// during sync"): Verify… opens the held challenge's page as a sheet of
// the Activity window and sends the token it closes with; closing it
// without one sends nothing; Try Again sends no token. The sheet here
// is a stand-in, so no page loads.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import Foundation
import SweetbriarBridge
import Testing

private actor RecordingSource: SyncVerificationSource {
    private(set) var sent: [(token: String?, tokenType: String?)] = []

    func protonVerification(accountId: String) async throws -> SyncVerificationChallenge? {
        SyncVerificationChallenge(
            url: "https://verify.proton.me/?methods=captcha&token=synthetic", methods: ["captcha"])
    }

    func verifyProtonAccount(accountId: String, token: String?, tokenType: String?) async throws {
        sent.append((token, tokenType))
    }
}

@MainActor
private final class StandInSheet: VerificationSheetPresenting {
    let url: URL
    let onMessage: @MainActor (HumanVerificationMessage) -> Void
    private(set) var parent: NSWindow?
    private(set) var closed = false

    init(url: URL, onMessage: @escaping @MainActor (HumanVerificationMessage) -> Void) {
        self.url = url
        self.onMessage = onMessage
    }

    func present(on parent: NSWindow) { self.parent = parent }
    func close() { closed = true }
}

/// The sheets the window made, in order.
@MainActor
private final class Sheets {
    var made: [StandInSheet] = []
}

@MainActor
private func until(_ condition: () async -> Bool) async throws {
    for _ in 0..<500 where !(await condition()) {
        try await Task.sleep(for: .milliseconds(10))
    }
}

@Suite("activity verification", .serialized)
@MainActor
struct ActivityVerificationTests {
    private func withWindow(
        _ body: @MainActor (ActivityWindowController, RecordingSource, () -> [StandInSheet]) async throws
            -> Void
    ) async throws {
        let root = FileManager.default.temporaryDirectory
            .appendingPathComponent("sb-activity-verify-\(UUID().uuidString)", isDirectory: true)
        defer { try? FileManager.default.removeItem(at: root) }
        let config = CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path)
        let bridge = try await CoreBridge.open(
            config: config, snapshots: FrameBatcher<[Setting]>(clock: ImmediateFrames()) { _ in })
        let source = RecordingSource()
        let sheets = Sheets()
        let controller = ActivityWindowController(
            model: ActivityModel(source: bridge), bridge: bridge,
            verification: SyncVerificationModel(source: source),
            makeSheet: { url, onMessage in
                let sheet = StandInSheet(url: url, onMessage: onMessage)
                sheets.made.append(sheet)
                return sheet
            },
            onSignInAgain: { _ in })
        controller.showWindow(nil)
        defer { controller.close() }
        try await body(controller, source, { sheets.made })
        await bridge.shutdown()
    }

    @Test(.timeLimit(.minutes(1))) func verifyOpensTheSheetAndSendsItsToken() async throws {
        try await withWindow { controller, source, sheets in
            controller.run(.verify, for: "proton")
            try await until { !sheets().isEmpty }
            let sheet = try #require(sheets().first)
            #expect(sheet.parent === controller.window, "a sheet of the Activity window")
            #expect(sheet.url.host == "verify.proton.me")

            sheet.onMessage(.verified(token: "answer", tokenType: "captcha"))
            #expect(sheet.closed)
            try await until { await !source.sent.isEmpty }
            let sent = await source.sent
            #expect(sent.count == 1 && sent.first?.token == "answer")
            #expect(sent.first?.tokenType == "captcha")
        }
    }

    @Test(.timeLimit(.minutes(1))) func closingTheSheetWithoutATokenSendsNothing() async throws {
        try await withWindow { controller, source, sheets in
            controller.run(.verify, for: "proton")
            try await until { !sheets().isEmpty }
            let sheet = try #require(sheets().first)
            sheet.onMessage(.closed)
            #expect(sheet.closed)
            try await Task.sleep(for: .milliseconds(100))
            #expect(await source.sent.isEmpty)
        }
    }

    @Test(.timeLimit(.minutes(1))) func tryAgainSendsNoTokenAndOpensNoSheet() async throws {
        try await withWindow { controller, source, sheets in
            controller.run(.tryAgain, for: "proton")
            try await until { await !source.sent.isEmpty }
            let sent = await source.sent
            #expect(sent.count == 1 && sent.first?.token == nil)
            #expect(sheets().isEmpty)
        }
    }
}

private struct ImmediateFrames: FrameClock {
    func nextFrame() async { try? await Task.sleep(for: .milliseconds(1)) }
}
#endif
