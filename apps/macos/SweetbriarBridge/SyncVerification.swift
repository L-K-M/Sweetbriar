// Answering Proton's human verification during sync (spec/04 "Human
// verification during sync", spec/08 "Verifying during sync"): the
// Activity row's Verify… reads the challenge the core holds and opens
// the sign-in verification sheet on it; the token the sheet closes
// with, or none for Try Again, goes to `verify_proton_account`. The
// challenge's URL and the token are secrets (spec/10): nothing here
// prints, logs or keeps them past the call that needs them.

import Foundation
import Observation

/// What an Activity row offers an account Proton asked to verify.
public enum SyncVerifyAction: Sendable, Equatable {
    /// The core holds Proton's challenge: open the sheet on it.
    case verify
    /// It doesn't (after a relaunch): start the worker again.
    case tryAgain

    public var title: String {
        switch self {
        case .verify: "Verify…"
        case .tryAgain: "Try Again"
        }
    }
}

/// The challenge the core holds for one account. Its URL carries
/// Proton's token, so its description never shows it.
public struct SyncVerificationChallenge: Sendable, Equatable, CustomStringConvertible {
    public var url: String
    public var methods: [String]

    public init(url: String, methods: [String]) {
        self.url = url
        self.methods = methods
    }

    public var description: String { "SyncVerificationChallenge(methods: \(methods))" }
}

/// The two core calls the Activity row needs.
public protocol SyncVerificationSource: Sendable {
    /// The challenge held for `accountId`, or nil when none is.
    func protonVerification(accountId: String) async throws -> SyncVerificationChallenge?
    /// Make the account active and start its worker: with the sheet's
    /// token, or without one for Try Again.
    func verifyProtonAccount(accountId: String, token: String?, tokenType: String?) async throws
}

extension CoreBridge: SyncVerificationSource {}

/// The Activity window's verification state: which accounts have a call
/// in flight, and the refusal each row shows until its next attempt.
@MainActor
@Observable
public final class SyncVerificationModel {
    public private(set) var busy: Set<String> = []
    public private(set) var refusals: [String: String] = [:]

    private let source: any SyncVerificationSource

    public init(source: any SyncVerificationSource) {
        self.source = source
    }

    /// The page to open for Verify…, or nil with the row's refusal set:
    /// the core no longer holds a challenge, or it names a page the
    /// sheet won't open.
    public func challengePage(accountId: String) async -> URL? {
        guard begin(accountId) else { return nil }
        defer { busy.remove(accountId) }
        do {
            guard let challenge = try await source.protonVerification(accountId: accountId) else {
                refusals[accountId] = "Proton's challenge is no longer here. Try Again."
                return nil
            }
            guard let url = HumanVerificationPolicy.startURL(challenge.url) else {
                refusals[accountId] = "Sweetbriar won't open the page Proton named."
                return nil
            }
            return url
        } catch {
            refusals[accountId] = error.localizedDescription
            return nil
        }
    }

    /// Send the sheet's token, or none for Try Again.
    public func verify(accountId: String, token: String? = nil, tokenType: String? = nil) async {
        guard begin(accountId) else { return }
        defer { busy.remove(accountId) }
        do {
            try await source.verifyProtonAccount(
                accountId: accountId, token: token, tokenType: tokenType)
        } catch {
            refusals[accountId] = error.localizedDescription
        }
    }

    /// One call per account at a time; a new attempt clears the row's
    /// last refusal.
    private func begin(_ accountId: String) -> Bool {
        guard !busy.contains(accountId) else { return false }
        busy.insert(accountId)
        refusals[accountId] = nil
        return true
    }
}
