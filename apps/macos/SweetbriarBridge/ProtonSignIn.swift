// Proton sign-in across the L1 boundary (spec/07 "Proton sign-in"):
// the bridge's own types, their mapping onto the generated ones, and
// the calls the sign-in sheet makes. App code never imports
// `SweetbriarCore`.

import Foundation
import SweetbriarCore

/// `begin_proton_sign_in`'s parameters (spec/07). The password is the
/// Proton password as UTF-8 bytes; the model never stores it.
public struct ProtonSignInRequest: Sendable, Equatable {
    public var displayName: String
    public var username: String
    public var password: Data
    /// Signs an existing Proton account in again, keeping its UUID and
    /// data (spec/07); `nil` starts a new account.
    public var accountId: String?

    public init(displayName: String, username: String, password: Data, accountId: String?) {
        self.displayName = displayName
        self.username = username
        self.password = password
        self.accountId = accountId
    }
}

extension ProtonSignInRequest: CustomStringConvertible, CustomDebugStringConvertible {
    /// Never the password (spec/10): logs and test failures print this.
    public var description: String {
        "ProtonSignInRequest(\(displayName), \(username), password: [redacted], "
            + "account: \(accountId ?? "new"))"
    }

    public var debugDescription: String { description }
}

/// One step's answer to a Proton challenge (spec/07). The mailbox
/// password crosses as bytes; the model zeroes its own copy once the
/// step is sent.
public enum ProtonSignInStep: Sendable, Equatable {
    case secondFactor(code: String)
    case mailboxPassword(bytes: Data)
    case humanVerification(token: String, tokenType: String)

    /// Map onto the generated enum case by case.
    var generated: SweetbriarCore.ProtonSignInStep {
        switch self {
        case .secondFactor(let code):
            return .secondFactor(code: code)
        case .mailboxPassword(let bytes):
            return .mailboxPassword(bytes: bytes)
        case .humanVerification(let token, let tokenType):
            return .humanVerification(token: token, tokenType: tokenType)
        }
    }
}

/// The next state of a Proton sign-in (spec/07).
public enum ProtonSignInState: Sendable, Equatable {
    case needsSecondFactor
    case needsMailboxPassword
    case needsHumanVerification(url: String, methods: [String])
    case complete(accountId: String)

    /// Map the generated enum case by case.
    init(_ generated: SweetbriarCore.ProtonSignInState) {
        switch generated {
        case .needsSecondFactor:
            self = .needsSecondFactor
        case .needsMailboxPassword:
            self = .needsMailboxPassword
        case .needsHumanVerification(let url, let methods):
            self = .needsHumanVerification(url: url, methods: methods)
        case .complete(let accountId):
            self = .complete(accountId: accountId)
        }
    }
}

/// `begin_proton_sign_in`'s answer: the open conversation, or a
/// replayed key's stored receipt after completion (spec/07).
public enum BeginProtonSignIn: Sendable, Equatable {
    case open(signInId: String, state: ProtonSignInState)
    case complete(receipt: Receipt)

    /// Map the generated enum case by case.
    init(_ generated: SweetbriarCore.BeginProtonSignIn) {
        switch generated {
        case .`open`(let signInId, let state):
            self = .open(signInId: signInId, state: ProtonSignInState(state))
        case .complete(let receipt):
            self = .complete(receipt: Receipt(receipt))
        }
    }
}

/// The sign-in calls the sheet needs (spec/07). `CoreBridge` adopts
/// it; tests substitute a fake.
public protocol ProtonSignInSource: Sendable {
    /// The command under a shell-made idempotency key.
    func beginProtonSignIn(
        idempotencyKey: String, params: ProtonSignInRequest
    ) async throws -> BeginProtonSignIn
    /// The next state for one challenge.
    func continueProtonSignIn(
        signInId: String, step: ProtonSignInStep
    ) async throws -> ProtonSignInState
    /// Drop the conversation (spec/07).
    func cancelProtonSignIn(signInId: String) async throws
    /// Revoke a Proton account's session, keeping its rows (spec/07).
    func disconnectAccount(accountId: String) async throws
}

extension ProtonSignInRequest {
    /// Map onto the generated record field by field.
    var generated: SweetbriarCore.ProtonSignInParams {
        SweetbriarCore.ProtonSignInParams(
            displayName: displayName,
            username: username,
            password: password,
            accountId: accountId
        )
    }
}
