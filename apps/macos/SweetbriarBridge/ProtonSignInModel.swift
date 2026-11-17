// `ProtonSignInModel`: the Proton sign-in sheet's state (plan M2.14a;
// spec/07 "Proton sign-in", spec/08 "Proton sign-in"), free of AppKit
// so the bridge tests exercise it.
//
//   credentials ─► begin ─┬─► secondFactor ─► submitCode ──────────┐
//                          ├─► mailboxPassword ─► submitMailboxPassword ─┤
//                          └─► humanVerification ─► submitVerification ──┤
//                                                                        ▼
//                                                                 done(accountID)
//
// A wrong password ends the conversation (spec/07): the model returns
// to credentials and every `begin` makes a fresh idempotency key. A
// wrong code or mailbox password keeps its step. Secrets are taken
// `inout` and the caller's buffer is zeroed when the call returns,
// sent or not (a busy model refuses without sending). The model
// keeps no copy. The copies the FFI makes on the way to the core's
// zeroizing secret are out of its reach (spec/10).
//
// One call runs at a time (`isBusy`). `cancel` bumps a generation, so
// an answer that lands after it changes nothing, and a conversation a
// cancelled `begin` opened anyway is dropped at the core.

import Foundation
import Observation

@MainActor
@Observable
public final class ProtonSignInModel {
    /// Where the conversation stands.
    public enum Step: Sendable, Equatable {
        case credentials
        case secondFactor
        case mailboxPassword
        case humanVerification(url: String, methods: [String])
        case done(accountID: String)
    }

    /// The last refusal, on the field it names, or Proton's message.
    public enum InlineError: Sendable, Equatable {
        case password(String)
        case secondFactor(String)
        case mailboxPassword(String)
        case account(String)
        /// The account's only second factor is a security key; the
        /// sheet links to Proton's 2FA settings (spec/08).
        case securityKey(String)
        case unavailable(String)
    }

    public private(set) var step: Step = .credentials
    public private(set) var inlineError: InlineError?

    /// A call is in flight; the sheet disables its controls.
    public private(set) var isBusy = false

    /// The username signing in again is fixed to (spec/08); empty for a
    /// new account, whose username the sheet passes to `begin`.
    public private(set) var username: String

    private let source: any ProtonSignInSource
    private let accountId: String?
    private var signInId: String?
    private var generation = 0

    public init(source: any ProtonSignInSource, accountId: String? = nil, username: String = "") {
        self.source = source
        self.accountId = accountId
        self.username = username
    }

    /// True when the sheet signs an existing account in again.
    public var isSigningInAgain: Bool { accountId != nil }

    /// Start a new account, or sign the existing one in again. A fresh
    /// idempotency key is made for every `begin`, so a wrong password's
    /// next attempt is a new SRP handshake (spec/07). The password is
    /// zeroed when `begin` returns, sent or not.
    public func begin(displayName: String, username: String, password: inout Data) async {
        defer { password.resetBytes(in: 0..<password.count) }
        guard let generation = start() else { return }
        defer { finish(generation) }
        do {
            // Built in the call, not kept, so nothing here outlives it.
            let answer = try await source.beginProtonSignIn(
                idempotencyKey: UUID().uuidString,
                params: ProtonSignInRequest(
                    displayName: displayName,
                    username: accountId == nil ? username : self.username,
                    password: password,
                    accountId: accountId
                )
            )
            guard generation == self.generation else {
                // Cancelled while the request ran: drop what it opened.
                if case .open(let signInId, _) = answer {
                    try? await source.cancelProtonSignIn(signInId: signInId)
                }
                return
            }
            switch answer {
            case .open(let signInId, let state):
                self.signInId = signInId
                apply(state)
            case .complete(let receipt):
                signInId = nil
                guard let accountID = Self.accountID(from: receipt) ?? accountId else {
                    inlineError = .unavailable("The sign-in finished without naming its account.")
                    return
                }
                step = .done(accountID: accountID)
            }
        } catch {
            guard generation == self.generation else { return }
            fail(error)
        }
    }

    /// Answer a second-factor challenge. A wrong code keeps the step.
    public func submitCode(_ code: String) async {
        await send(.secondFactor(code: code))
    }

    /// Answer the mailbox password. The bytes are zeroed when this
    /// returns, sent or not.
    public func submitMailboxPassword(_ password: inout Data) async {
        defer { password.resetBytes(in: 0..<password.count) }
        await send(.mailboxPassword(bytes: password))
    }

    /// Answer human verification with the token Proton handed back.
    public func submitVerification(token: String, tokenType: String) async {
        await send(.humanVerification(token: token, tokenType: tokenType))
    }

    /// Drop the conversation when one is open (spec/07). The sheet is
    /// closing, so a cancel that cannot reach the core is not fatal.
    /// A call in flight is abandoned: its answer changes nothing.
    public func cancel() async {
        generation += 1
        isBusy = false
        inlineError = nil
        guard let signInId else { return }
        self.signInId = nil
        step = .credentials
        try? await source.cancelProtonSignIn(signInId: signInId)
    }

    private func send(_ challenge: ProtonSignInStep) async {
        guard let signInId, let generation = start() else { return }
        defer { finish(generation) }
        do {
            let state = try await source.continueProtonSignIn(signInId: signInId, step: challenge)
            guard generation == self.generation else { return }
            apply(state)
        } catch {
            guard generation == self.generation else { return }
            fail(error)
        }
    }

    /// Claims the model for one call; `nil` while another runs.
    private func start() -> Int? {
        guard !isBusy else { return nil }
        isBusy = true
        inlineError = nil
        return generation
    }

    /// Releases the claim, unless a `cancel` already did.
    private func finish(_ generation: Int) {
        if generation == self.generation {
            isBusy = false
        }
    }

    private func apply(_ state: ProtonSignInState) {
        switch state {
        case .needsSecondFactor:
            step = .secondFactor
        case .needsMailboxPassword:
            step = .mailboxPassword
        case .needsHumanVerification(let url, let methods):
            step = .humanVerification(url: url, methods: methods)
        case .complete(let accountId):
            signInId = nil
            step = .done(accountID: accountId)
        }
    }

    /// Show the refusal on the field it names. A wrong password ends
    /// the conversation and returns to credentials (spec/07); the next
    /// `begin` makes a new idempotency key.
    private func fail(_ error: any Error) {
        if error is CancellationError {
            // The caller's task ended; the sheet keeps what it typed.
            return
        }
        guard let error = error as? CoreFailure else {
            inlineError = .unavailable(error.localizedDescription)
            return
        }
        switch error {
        case .invalidInput(let field, let message):
            switch field {
            case "password":
                signInId = nil
                step = .credentials
                inlineError = .password(message)
            case "second_factor":
                inlineError = .secondFactor(message)
            case "mailbox_password":
                inlineError = .mailboxPassword(message)
            case "account":
                inlineError = .account(message)
            case "security_key":
                inlineError = .securityKey(message)
            default:
                inlineError = .unavailable(message)
            }
        case .unavailable(let reason):
            inlineError = .unavailable(reason)
        default:
            inlineError = .unavailable(error.errorDescription ?? "The sign-in failed.")
        }
    }

    /// The account id a completed sign-in's receipt stores
    /// (`{"account_id": ...}`, spec/07).
    private static func accountID(from receipt: Receipt) -> String? {
        guard let data = receipt.resultJson.data(using: .utf8),
              let object = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let accountId = object["account_id"] as? String
        else { return nil }
        return accountId
    }
}
