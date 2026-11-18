// The Proton sign-in sheet (plan M2.14b, spec/08 "Proton sign-in"): a
// sheet ended from outside, as when its window goes, cancels its
// conversation at the core rather than leaving it running unseen.

import AppKit
import Foundation
import Testing

@testable import SweetbriarBridge

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

/// Opens one conversation at the second factor and records cancels.
private final class OpenConversationSource: ProtonSignInSource, @unchecked Sendable {
    private let lock = NSLock()
    private var _cancels: [String] = []

    var cancels: [String] { lock.withLock { _cancels } }

    func beginProtonSignIn(
        idempotencyKey: String, params: ProtonSignInRequest
    ) async throws -> BeginProtonSignIn {
        .open(signInId: "sign-in-1", state: .needsSecondFactor)
    }

    func continueProtonSignIn(
        signInId: String, step: ProtonSignInStep
    ) async throws -> ProtonSignInState {
        throw CoreFailure.unavailable(reason: "unused")
    }

    func cancelProtonSignIn(signInId: String) async throws {
        lock.withLock { _cancels.append(signInId) }
    }

    func disconnectAccount(accountId: String) async throws {}
}

@MainActor
@Suite("Proton sign-in sheet")
struct ProtonSignInSheetTests {
    @Test func aSheetEndedFromOutsideCancelsItsConversation() async throws {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 640, height: 480),
            styleMask: [.titled], backing: .buffered, defer: false
        )
        window.isReleasedWhenClosed = false
        window.orderFront(nil)
        defer { window.close() }
        let source = OpenConversationSource()
        let signIn = ProtonSignInSheet(
            window: window, source: source, writes: ArchiveWriteTasks(), canWrite: { true }
        )

        signIn.begin()
        let form = try #require(signIn.form)
        form.displayName = "Owner"
        form.username = "owner@example.org"
        form.password = "synthetic-proton-password"
        await form.submit()
        #expect(form.model.step == .secondFactor)

        let sheet = try #require(signIn.sheet)
        window.endSheet(sheet)
        for _ in 0..<200 where source.cancels.isEmpty {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(source.cancels == ["sign-in-1"])
        #expect(signIn.form == nil)
        #expect(signIn.sheet == nil)
    }
}
